// SPDX-License-Identifier: GPL-2.0-or-later
//! Direct-MAIN execution with byte coverage, architectural TCM and exact MMIO.
//! Missing peripherals stop the CPU; no guest OS state or ROM results are seeded.
use crate::{keys::Keypad, registers::Registers};
use anyhow::{ensure, Context, Result};
use e830::core::{bus, cpu::{self, ArmCpu, ArmCpuModel, CpuConfig, EngineProt, HookType, Mode, RegisterARM, Unicorn},
    report::{ExecutionReport, MemoryFault, MmioAccess, StopReason}};
use e830::devices::runtime::{DeviceRuntime, Frame, RunReport, RuntimeOptions};
use e830::firmware::LoadedFirmware;
use serde::Deserialize;
use std::{any::Any, collections::VecDeque};
use unicorn_engine::{RegisterARMCP, TlbType};

const NOR: u64 = 0x4400_0000;
const NOR_SIZE: u64 = 0x0200_0000;
const RAM: u64 = 0x4c00_0000;
const TABLE: u64 = 0xfe00_0000;

struct Memory {
    base: u64,
    known: Vec<u8>,
    writable: bool,
    itcm: bool,
}
impl Memory {
    fn offset(&self, address: u64, size: usize) -> Option<usize> {
        address.checked_sub(self.base).and_then(|offset| {
            offset.checked_add(size as u64).filter(|end| *end <= self.known.len() as u64).map(|_| offset as usize)
        })
    }
}
struct Proof { address: u64, expected: Vec<u8> }
#[derive(Deserialize)]
struct CopyContract { mappings: Vec<CopyMapping> }
#[derive(Deserialize)]
struct CopyMapping { address: u64, source: u64, size: usize, sha256: String }

struct State {
    memory: Vec<Memory>,
    code: Vec<(u64, u64)>,
    proofs: Vec<Proof>,
    registers: Registers,
    keys: Keypad,
    tcm: [u32; 2],
    sctlr: u32,
    ttbr: u32,
    instructions: u64,
    halt: Option<String>,
    fault: Option<MemoryFault>,
    recent: VecDeque<MmioAccess>,
    trace: Vec<MmioAccess>,
    trace_mmio: bool,
    breakpoint: Option<(u32, u64)>,
    at_breakpoint: bool,
}

fn stop(uc: &mut Unicorn<'_, State>, reason: String, address: u64, size: usize) {
    if uc.get_data().halt.is_none() {
        let pc = uc.reg_read(RegisterARM::PC).unwrap_or(0) as u32;
        uc.get_data_mut().fault = Some(MemoryFault { kind: reason.clone(), address, size, pc });
        uc.get_data_mut().halt = Some(reason);
    }
    let _ = uc.emu_stop();
}
fn record(uc: &mut Unicorn<'_, State>, write: bool, address: u32, size: usize, value: u64) {
    let access = MmioAccess { write, address, size, value, pc: uc.reg_read(RegisterARM::PC).unwrap_or(0) as u32 };
    let state = uc.get_data_mut();
    if state.recent.len() == 32 { state.recent.pop_front(); }
    state.recent.push_back(access.clone());
    if state.trace_mmio && state.trace.len() < 4096 { state.trace.push(access); }
}

fn checked_memory(uc: &mut Unicorn<'_, State>, address: u64, size: usize, write: bool) {
    let index = uc.get_data().memory.iter().position(|region| region.offset(address, size).is_some());
    let Some(index) = index else { return; }; // MMIO has its own decoder.
    let region = &uc.get_data().memory[index];
    let offset = region.offset(address, size).unwrap();
    if write {
        if !region.writable {
            stop(uc, "read_only_storage_write_unmodeled".into(), address, size);
        } else {
            uc.get_data_mut().memory[index].known[offset..offset + size].fill(1);
        }
    } else if let Some(unknown) = region.known[offset..offset + size].iter().position(|known| *known == 0) {
        stop(uc, "read_unknown_byte".into(), address + unknown as u64, 1);
    }
}

fn add_memory(uc: &mut Unicorn<'_, State>, base: u64, size: u64, writable: bool, fill_known: bool, itcm: bool) -> Result<()> {
    ensure!(base & 0xfff == 0 && size > 0 && size & 0xfff == 0 && base.checked_add(size).is_some_and(|end| end <= 1u64 << 32), "Invalid DB2010 memory range");
    ensure!(uc.get_data().memory.iter().all(|r| base + size <= r.base || r.base + r.known.len() as u64 <= base), "TCM or image mapping overlaps another region");
    uc.mem_map(base, size, if writable { EngineProt::ALL } else { EngineProt::READ | EngineProt::EXEC })
        .map_err(|error| anyhow::anyhow!("mapping 0x{base:08x}: {error:?}"))?;
    uc.get_data_mut().memory.push(Memory { base, known: vec![u8::from(fill_known); size as usize], writable, itcm });
    Ok(())
}

fn word_register(rd: usize) -> RegisterARM {
    [RegisterARM::R0, RegisterARM::R1, RegisterARM::R2, RegisterARM::R3,
        RegisterARM::R4, RegisterARM::R5, RegisterARM::R6, RegisterARM::R7,
        RegisterARM::R8, RegisterARM::R9, RegisterARM::R10, RegisterARM::R11,
        RegisterARM::R12, RegisterARM::SP, RegisterARM::LR, RegisterARM::PC][rd]
}

fn cp15(uc: &mut Unicorn<'_, State>, pc: u64, insn: u32, cpsr: u32) -> Result<()> {
    if !cpu::arm_condition_passed(insn >> 28, cpsr) { return Ok(()); }
    let op = insn & 0x0fff_0fff;
    let rd = ((insn >> 12) & 15) as usize;
    let input = if rd == 15 { pc as u32 + 8 } else { uc.reg_read(word_register(rd)).map_err(|e| anyhow::anyhow!("CP15 operand: {e:?}"))? as u32 };
    if op == 0x0e10_0f50 || matches!(op, 0x0e09_0f11 | 0x0e09_0f31 | 0x0e19_0f11 | 0x0e19_0f31) {
        let mut output = None;
        if op == 0x0e10_0f50 { output = Some(0x10001); }
        else {
            let bank = ((insn >> 5) & 1) as usize;
            if insn & (1 << 20) != 0 { output = Some(uc.get_data().tcm[bank]); }
            else {
                let base = input & 0xffff_f000;
                let enabled = input & 1 != 0;
                ensure!(rd != 15 && input & 0xfc2 == 0 && base & 0xffff == 0, "cp15_tcm_encoding_unmodeled");
                let old = uc.get_data().tcm[bank];
                ensure!(old & 1 == 0 || (enabled && base == old & 0xffff_f000), "cp15_tcm_mapping_change_unmodeled");
                if enabled && old & 1 == 0 {
                    ensure!(uc.get_data().sctlr & 1 == 0, "cp15_tcm_mmu_mapping_unmodeled");
                    add_memory(uc, u64::from(base), 0x10000, true, false, bank == 1)?;
                }
                uc.get_data_mut().tcm[bank] = base | 0x1c | u32::from(enabled);
            }
        }
        if let Some(value) = output {
            if rd == 15 {
                uc.reg_write(RegisterARM::CPSR, u64::from((cpsr & 0x0fff_ffff) | (value & 0xf000_0000))).map_err(|e| anyhow::anyhow!("MRC flags: {e:?}"))?;
            } else { uc.reg_write(word_register(rd), u64::from(value)).map_err(|e| anyhow::anyhow!("TCM result: {e:?}"))?; }
        }
        uc.reg_write(RegisterARM::PC, pc + 4).map_err(|e| anyhow::anyhow!("TCM continuation: {e:?}"))?;
        return Ok(());
    }
    if op == 0x0e0d_0f10 { ensure!(rd != 15 && input == 0, "section_table_cpu_mapping_unmodeled"); }
    if op == 0x0e02_0f10 {
        ensure!(uc.get_data().sctlr & 1 == 0 || (rd != 15 && input == TABLE as u32), "section_table_cpu_mapping_unmodeled");
        uc.get_data_mut().ttbr = input;
    }
    let enabling = op == 0x0e01_0f10 && input & 1 != 0;
    if op == 0x0e01_0f10 { ensure!(rd != 15 && input & 0x80 == 0, "cp15_big_endian_mapping_unmodeled"); }
    if enabling || uc.get_data().sctlr & 1 != 0 {
        ensure!(uc.get_data().ttbr == TABLE as u32, "section_table_cpu_mapping_unmodeled");
        if uc.get_data().registers.table_dirty && (enabling || op == 0x0e08_0f17) {
            let table = uc.get_data().registers.section_table()?;
            uc.mem_write(TABLE, &table).map_err(|e| anyhow::anyhow!("publishing section table: {e:?}"))?;
            let index = uc.get_data().memory.iter().position(|r| r.base == TABLE).unwrap();
            uc.get_data_mut().memory[index].known.fill(1);
            uc.get_data_mut().registers.table_dirty = false;
        }
    }
    if op == 0x0e01_0f10 { uc.get_data_mut().sctlr = input; }
    Ok(()) // Unicorn executes the other architectural coprocessor operations.
}

pub struct K750Machine { cpu: ArmCpu<State> }

impl K750Machine {
    pub fn new(firmware: LoadedFirmware, options: RuntimeOptions) -> Result<Self> {
        firmware.validate()?;
        let mut code = Vec::new();
        for segment in &firmware.metadata.segments {
            if segment.role == "image" || segment.role == "rom" { code.push((segment.address, segment.address + segment.size as u64)); }
        }
        let state = State { memory: vec![], code, proofs: vec![], registers: Registers::new()?, keys: Keypad::default(),
            tcm: [0x1c; 2], sctlr: 0x90078, ttbr: 0, instructions: 0, halt: None, fault: None,
            recent: VecDeque::new(), trace: vec![], trace_mmio: options.trace_mmio, breakpoint: None, at_breakpoint: false };
        let mut machine = Self { cpu: ArmCpu::new_with_data(CpuConfig {
            mode: Mode::ARM | Mode::LITTLE_ENDIAN, model: Some(ArmCpuModel::Model_926), initial_registers: &[],
        }, state).map_err(|e| anyhow::anyhow!("creating ARM926: {e:?}"))? };
        let uc = machine.cpu.raw_engine();
        uc.ctl_set_tlb_type(TlbType::CPU).map_err(|e| anyhow::anyhow!("selecting architectural MMU: {e:?}"))?;
        let mut control = RegisterARMCP::new().cp(15).crn(1);
        control.val = 0x90078;
        uc.reg_write_arm_coproc(&control).map_err(|e| anyhow::anyhow!("setting initial SCTLR: {e:?}"))?;
        add_memory(uc, NOR, NOR_SIZE, false, false, false)?;
        add_memory(uc, RAM, 0x800000, true, true, false)?; // Declared upstream RAM initialization assumption.
        add_memory(uc, 0xf3000000, 0x1000, true, false, false)?;
        add_memory(uc, 0xf6000000, 0x3000, true, false, false)?;
        add_memory(uc, TABLE, 0x4000, false, false, false)?;
        if firmware.input.image("rom").is_some() { add_memory(uc, 0xffff0000, 0x10000, false, false, false)?; }
        for segment in &firmware.metadata.segments {
            let index = uc.get_data().memory.iter().position(|r| r.offset(segment.address, segment.size).is_some())
                .context("Firmware record is outside the DB2010 memory map")?;
            let offset = uc.get_data().memory[index].offset(segment.address, segment.size).unwrap();
            ensure!(uc.get_data().memory[index].known[offset..offset + segment.size].iter().all(|v| *v == 0), "Firmware records overlap");
            uc.mem_write(segment.address, firmware.segment(segment)?).map_err(|e| anyhow::anyhow!("loading original firmware: {e:?}"))?;
            uc.get_data_mut().memory[index].known[offset..offset + segment.size].fill(1);
        }
        if firmware.metadata.image_sha256 == "048999c26718a39afb6bbce268b5bfee7cf70da679c8bd5010f1fd70f1eadbf2" {
            let contract: CopyContract = serde_json::from_str(include_str!("../../../src/db2010/resources/k750-r1db001-cid49-execution-contract.json"))?;
            for row in contract.mappings {
                let mut expected = vec![0; row.size];
                uc.mem_read(row.source, &mut expected).map_err(|e| anyhow::anyhow!("reading original copy proof: {e:?}"))?;
                ensure!(e830::firmware::image_sha256(&expected) == row.sha256, "Original RAM copy proof differs");
                uc.get_data_mut().proofs.push(Proof { address: row.address, expected });
            }
        }
        for page in uc.get_data().registers.pages() {
            ensure!(!uc.get_data().memory.iter().any(|r| r.offset(u64::from(page), 4096).is_some()), "Register bank overlaps RAM");
            bus::map_bank(uc, u64::from(page), 4096,
                Some(move |uc: &mut Unicorn<'_, State>, offset: u64, size: usize| {
                    let address = page + offset as u32;
                    let result = if page == 0xfb010000 { uc.get_data().keys.read(address, size) } else { uc.get_data().registers.read(address, size) };
                    match result {
                        Ok(value) => { record(uc, false, address, size, value); value }
                        Err(error) => { record(uc, false, address, size, 0); stop(uc, error.to_string(), address as u64, size); 0 }
                    }
                }), Some(move |uc: &mut Unicorn<'_, State>, offset: u64, size: usize, value: u64| {
                    let address = page + offset as u32;
                    record(uc, true, address, size, value);
                    let result = if page == 0xfb010000 { uc.get_data_mut().keys.write(address, size, value) } else { uc.get_data_mut().registers.write(address, size, value) };
                    if let Err(error) = result { stop(uc, error.to_string(), address as u64, size); }
                })).map_err(|e| anyhow::anyhow!("mapping DB2010 bus: {e:?}"))?;
        }
        uc.add_mem_hook(HookType::MEM_READ | HookType::MEM_WRITE, 1, 0, |uc, kind, address, size, _| {
            checked_memory(uc, address, size, kind == unicorn_engine::MemType::WRITE);
            false
        }).map_err(|e| anyhow::anyhow!("installing byte coverage: {e:?}"))?;
        uc.add_mem_hook(HookType::MEM_INVALID, 1, 0, |uc, kind, address, size, _| {
            stop(uc, format!("{kind:?}"), address, size); false
        }).map_err(|e| anyhow::anyhow!("installing memory diagnostics: {e:?}"))?;
        uc.add_intr_hook(|uc, exception| {
            let pc = uc.reg_read(RegisterARM::PC).unwrap_or(0);
            stop(uc, format!("cpu_exception_unmodeled_{exception}"), pc, 4);
        }).map_err(|e| anyhow::anyhow!("installing exception guard: {e:?}"))?;
        uc.add_code_hook(1, 0, |uc, pc, size| {
            if uc.get_data().halt.is_some() { let _ = uc.emu_stop(); return; }
            if uc.get_data().breakpoint.is_some_and(|(address, after)| u64::from(address) == pc && uc.get_data().instructions >= after) {
                uc.get_data_mut().at_breakpoint = true; let _ = uc.emu_stop(); return;
            }
            let size = size as usize;
            let region = uc.get_data().memory.iter().find(|r| r.offset(pc, size).is_some());
            let Some(region) = region else { stop(uc, "execute_unmapped".into(), pc, size); return; };
            let offset = region.offset(pc, size).unwrap();
            if region.known[offset..offset + size].contains(&0) { stop(uc, "execute_unknown_byte".into(), pc, size); return; }
            let proof = uc.get_data().proofs.iter().find(|p| pc >= p.address && pc + size as u64 <= p.address + p.expected.len() as u64);
            let approved = region.itcm || proof.is_some() || uc.get_data().code.iter().any(|&(lo, hi)| lo <= pc && pc + size as u64 <= hi);
            if !approved { stop(uc, "execute_outside_original_code".into(), pc, size); return; }
            let mut instruction = vec![0; size];
            if let Err(error) = uc.mem_read(pc, &mut instruction) { stop(uc, format!("fetching instruction: {error:?}"), pc, size); return; }
            if let Some(proof) = proof {
                let offset = (pc - proof.address) as usize;
                if instruction != proof.expected[offset..offset + size] { stop(uc, "ram_original_copy_changed".into(), pc, size); return; }
            }
            let cpsr = uc.reg_read(RegisterARM::CPSR).unwrap_or(0) as u32;
            if size == 4 && cpsr & cpu::CPSR_THUMB == 0 {
                let insn = u32::from_le_bytes(instruction.try_into().unwrap());
                if let Err(error) = cp15(uc, pc, insn, cpsr) { stop(uc, error.to_string(), pc, size); return; }
            }
            uc.get_data_mut().instructions += 1;
        }).map_err(|e| anyhow::anyhow!("installing original-code guard: {e:?}"))?;
        uc.reg_write(RegisterARM::CPSR, 0xd3).map_err(|e| anyhow::anyhow!("setting CPSR: {e:?}"))?;
        uc.reg_write(RegisterARM::PC, u64::from(firmware.metadata.entry)).map_err(|e| anyhow::anyhow!("setting MAIN entry: {e:?}"))?;
        Ok(machine)
    }

    pub fn read_memory(&mut self, address: u64, size: usize) -> Result<Vec<u8>> {
        ensure!(size <= NOR_SIZE as usize, "Diagnostic read exceeds memory bound");
        let mut data = vec![0; size];
        self.cpu.raw_engine().mem_read(address, &mut data).map_err(|e| anyhow::anyhow!("diagnostic read: {e:?}"))?;
        Ok(data)
    }

    fn report(&mut self, error: Option<String>) -> RunReport {
        let uc = self.cpu.raw_engine();
        let reg = |r| uc.reg_read(r).unwrap_or(0) as u32;
        let state = uc.get_data();
        let stop_reason = if let Some(reason) = state.halt.clone().or(error) { StopReason::CpuError(reason) }
            else if state.at_breakpoint { StopReason::Breakpoint } else { StopReason::InstructionBudget };
        RunReport { device: "k750i".into(), lcd_frames: 0, execution: ExecutionReport {
            instructions: state.instructions, fast_forwarded_instructions: 0,
            pc: reg(RegisterARM::PC), cpsr: reg(RegisterARM::CPSR), r0: reg(RegisterARM::R0),
            r1: reg(RegisterARM::R1), r2: reg(RegisterARM::R2), r3: reg(RegisterARM::R3),
            r4: reg(RegisterARM::R4), r5: reg(RegisterARM::R5), r6: reg(RegisterARM::R6), r7: reg(RegisterARM::R7),
            sp: reg(RegisterARM::SP), lr: reg(RegisterARM::LR), stop_reason, memory_fault: state.fault.clone(),
            recent_mmio: state.recent.iter().cloned().collect(),
        }, details: serde_json::json!({
            "startup": "direct MAIN reset handler; boot ROM handoff unresolved",
            "cpu": "Unicorn ARM926 with architectural CPU MMU", "firmware_patches": 0, "rom_hle": false,
            "undetectability": "unverified", "instruction_counter": "pre-instruction observations; not a cycle count",
            "sctlr": state.sctlr, "ttbr": state.ttbr, "tcm": state.tcm,
            "controller_words": state.registers.controller, "region_words": state.registers.regions,
            "device_registers": state.registers.established(), "mmio_trace": state.trace,
            "uart_bytes": state.registers.uart.iter().map(Vec::len).collect::<Vec<_>>(),
            "key_transitions": state.keys.transitions,
            "assumptions": ["Zeroed external RAM inherited from upstream", "Register reset values and read masks inherited from the DB2010 research profile", "Ideal keypad contacts and immediate UART drain"],
            "unimplemented": ["NOR command engine", "NAND and GDFS provisioning", "Timers and interrupt routing", "LCD/PDI and DMA", "I2C/PMIC", "DSP/audio", "SIM and RF", "USB", "Camera"],
        }) }
    }
}

impl DeviceRuntime for K750Machine {
    fn run(&mut self, budget: u64) -> Result<RunReport> {
        ensure!(budget > 0, "K750 instruction budget must be nonzero");
        let uc = self.cpu.raw_engine();
        if uc.get_data().halt.is_some() { return Ok(self.report(None)); }
        uc.get_data_mut().at_breakpoint = false;
        let pc = uc.reg_read(RegisterARM::PC).map_err(|e| anyhow::anyhow!("reading PC: {e:?}"))?;
        let cpsr = uc.reg_read(RegisterARM::CPSR).map_err(|e| anyhow::anyhow!("reading CPSR: {e:?}"))?;
        let start = pc | u64::from(cpsr & 0x20 != 0);
        let outcome = uc.emu_start(start, 1u64 << 32, 0, usize::try_from(budget).unwrap_or(usize::MAX));
        if let Err(error) = outcome {
            if uc.get_data().halt.is_none() { uc.get_data_mut().halt = Some(format!("{error:?}")); }
        }
        Ok(self.report(None))
    }
    fn frame(&mut self) -> Option<Frame> { None }
    fn frame_revision(&mut self) -> u64 { 0 }
    fn set_phone_keys(&mut self, keys: u32) -> Result<()> { self.cpu.raw_engine().get_data_mut().keys.set(keys) }
    fn set_breakpoint(&mut self, pc: u32, after: u64) { self.cpu.raw_engine().get_data_mut().breakpoint = Some((pc & !1, after)); }
    fn as_any_mut(&mut self) -> &mut dyn Any { self }
}

#[cfg(test)]
mod tests {
    use super::*;
    use e830::core::memory::Permissions;
    use e830::firmware::{FirmwareInput, FirmwareMetadata, ImageSegment};
    use std::collections::BTreeMap;
    fn program(words: &[(usize, u32)]) -> LoadedFirmware {
        let mut image = vec![0; 0x100];
        for &(offset, word) in words { image[offset..offset + 4].copy_from_slice(&word.to_le_bytes()); }
        LoadedFirmware { metadata: FirmwareMetadata {
            loader: "synthetic ARM fixture".into(), format: crate::loader::FORMAT.into(), revision: None,
            image_sha256: e830::firmware::image_sha256(&image), image_size: image.len(), entry: 0x44020040,
            instruction_set: "ARM".into(), facts: BTreeMap::new(), limitations: vec![],
            segments: vec![ImageSegment { name: "Original test instructions".into(), role: "image".into(), offset: 0,
                size: image.len(), address: 0x44020000, permissions: Permissions::RX }],
        }, input: FirmwareInput { image, companions: BTreeMap::new() } }
    }
    #[test]
    fn real_arm_execution_uses_the_bus_and_stops_on_unknown_registers() {
        let firmware = program(&[(0x40, 0xe59f0020), (0x44, 0xe3a0107b), (0x48, 0xe5801000),
            (0x4c, 0xe5902000), (0x50, 0xe5903004), (0x54, 0xeafffffe), (0x68, 0x14000000)]);
        let mut machine = K750Machine::new(firmware, RuntimeOptions::default()).unwrap();
        let report = machine.run(100).unwrap();
        assert_eq!(report.execution.r2, 123);
        let fault = report.execution.memory_fault.unwrap();
        assert_eq!((fault.address, fault.pc), (0x14000004, 0x44020050));
        assert!(machine.frame().is_none());
        assert_eq!(machine.run(100).unwrap().execution.instructions, report.execution.instructions);
    }
    #[test]
    fn arm_thumb_switch_and_resumed_quanta_preserve_registers() {
        let words = [(0x40, 0xe59f0020), (0x44, 0xe12fff10), (0x68, 0x44020081), (0x80, 0xe7fe2107)];
        let mut one = K750Machine::new(program(&words), RuntimeOptions::default()).unwrap();
        let mut split = K750Machine::new(program(&words), RuntimeOptions::default()).unwrap();
        let final_one = one.run(20).unwrap();
        for _ in 0..4 { split.run(5).unwrap(); }
        let final_split = split.report(None);
        assert_eq!(final_one.execution.r1, 7);
        assert_ne!(final_one.execution.cpsr & 0x20, 0);
        assert_eq!(serde_json::to_value(final_one).unwrap(), serde_json::to_value(final_split).unwrap());
    }
    #[test]
    fn linked_cpu_backend_matches_the_pinned_source_version() {
        // The upstream Rust build accepts any system Unicorn >= 2. That
        // silently selected 2.1.1 here, whose AArch64 JIT return address was
        // shared by all engines. Rust package 2.1.5 bundles C API 2.1.4;
        // check the linked implementation, not the Rust package constant.
        let version = unsafe { unicorn_engine::uc_version(std::ptr::null_mut(), std::ptr::null_mut()) };
        assert_eq!(version >> 8, 0x020104, "Build the pinned Unicorn Rust 2.1.5 bundle (C API 2.1.4); an older system library can crash independent phone sessions");
    }
    #[test]
    fn closing_another_cpu_keeps_the_surviving_jit_and_thumb_state_valid() {
        let words = [(0x40, 0xe59f0020), (0x44, 0xe12fff10), (0x68, 0x44020081), (0x80, 0xe7fe2107)];
        let mut surviving = K750Machine::new(program(&words), RuntimeOptions::default()).unwrap();
        let discarded = K750Machine::new(program(&words), RuntimeOptions::default()).unwrap();
        drop(discarded);
        let first = surviving.run(20).unwrap();
        assert_eq!(first.execution.r1, 7);
        assert_ne!(first.execution.cpsr & 0x20, 0);
        assert!(first.execution.memory_fault.is_none());
        // Repeat after translation, to cover both new and cached TB exits.
        let discarded = K750Machine::new(program(&words), RuntimeOptions::default()).unwrap();
        drop(discarded);
        let resumed = surviving.run(5).unwrap();
        assert_eq!(resumed.execution.instructions, first.execution.instructions + 5);
        assert_eq!(resumed.execution.r1, 7);
        assert_ne!(resumed.execution.cpsr & 0x20, 0);
        assert!(resumed.execution.memory_fault.is_none());
    }
    #[test]
    fn sparse_flash_holes_cannot_be_executed_as_zero_filled_code() {
        let mut firmware = program(&[(0x40, 0xe59ff020), (0x68, 0x44020200)]);
        firmware.metadata.entry = 0x44020040;
        let mut machine = K750Machine::new(firmware, RuntimeOptions::default()).unwrap();
        let report = machine.run(10).unwrap();
        assert!(matches!(report.execution.stop_reason, StopReason::CpuError(ref s) if s == "execute_unknown_byte"));
        assert_eq!(report.execution.memory_fault.unwrap().address, 0x44020200);
    }
}
