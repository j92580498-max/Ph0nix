//! Reset execution, memory wiring and architectural exceptions for the OneC board.
//! No revision-specific PC hooks, replacement instructions or seeded RTOS state.
use super::{
    memory::{NOR_BASES, NOR_SIZE, RAM_BASE, RAM_SIZE},
    nor::Nor,
    soc::{self, Soc},
};
use crate::core::{
    bus,
    cpu::{self, ArmCpu, ArmCpuModel, CpuConfig, EngineProt, HookType, Mode, RegisterARM, Unicorn},
    report::{ExecutionReport, MemoryFault, MmioAccess, StopReason},
};
use crate::devices::runtime::{DeviceRuntime, Frame, RunReport, RuntimeOptions};
use crate::firmware::LoadedFirmware;
use anyhow::{ensure, Context, Result};
use std::any::Any;
use std::collections::VecDeque;

struct State {
    nor: [Nor; 2],
    soc: Soc,
    restore: Vec<(usize, usize, usize)>,
    instructions: u64,
    irqs: u64,
    svcs: u64,
    halt: Option<String>,
    memory_fault: Option<MemoryFault>,
    recent_mmio: VecDeque<MmioAccess>,
    trace: Vec<MmioAccess>,
    trace_mmio: bool,
    stop_on_frame: bool,
    frame_stop: bool,
    breakpoint: Option<(u32, u64)>,
    at_breakpoint: bool,
}

impl State {
    fn record(&mut self, access: MmioAccess) {
        if self.recent_mmio.len() == 32 {
            self.recent_mmio.pop_front();
        }
        self.recent_mmio.push_back(access.clone());
        if self.trace_mmio && self.trace.len() < 4096 {
            self.trace.push(access);
        }
    }
}

pub struct N500Machine {
    cpu: ArmCpu<State>,
}

fn stop(uc: &mut Unicorn<'_, State>, reason: String) {
    if uc.get_data().halt.is_none() {
        uc.get_data_mut().halt = Some(reason);
    }
    let _ = uc.emu_stop();
}

impl N500Machine {
    pub fn new(firmware: LoadedFirmware, options: RuntimeOptions) -> Result<Self> {
        firmware.validate()?;
        let mut nor = [Nor::erased(), Nor::erased()];
        for segment in &firmware.metadata.segments {
            let bytes = firmware.segment(segment)?;
            let (bank, offset) = NOR_BASES
                .iter()
                .enumerate()
                .find_map(|(index, base)| {
                    segment
                        .address
                        .checked_sub(*base)
                        .filter(|offset| {
                            offset
                                .checked_add(bytes.len() as u64)
                                .is_some_and(|end| end <= NOR_SIZE as u64)
                        })
                        .map(|offset| (index, offset as usize))
                })
                .context("N500 image segment is outside the two NOR banks")?;
            nor[bank].cells[offset..offset + bytes.len()].copy_from_slice(bytes);
        }
        let state = State {
            nor,
            soc: Soc::new(firmware.input.image("eeprom").map(<[u8]>::to_vec)),
            restore: Vec::new(),
            instructions: 0,
            irqs: 0,
            svcs: 0,
            halt: None,
            memory_fault: None,
            recent_mmio: VecDeque::with_capacity(32),
            trace: Vec::new(),
            trace_mmio: options.trace_mmio,
            stop_on_frame: options.stop_on_frame,
            frame_stop: false,
            breakpoint: None,
            at_breakpoint: false,
        };
        let config = CpuConfig {
            mode: Mode::ARM | Mode::LITTLE_ENDIAN,
            model: Some(ArmCpuModel::TI925T),
            initial_registers: &[],
        };
        let mut machine = Self {
            cpu: ArmCpu::new_with_data(config, state)
                .map_err(|error| anyhow::anyhow!("creating N500 ARM CPU: {error:?}"))?,
        };
        let uc = machine.cpu.raw_engine();
        uc.mem_map(RAM_BASE, RAM_SIZE, EngineProt::ALL)
            .map_err(|error| anyhow::anyhow!("mapping N500 RAM: {error:?}"))?;
        for (bank, base) in NOR_BASES.into_iter().enumerate() {
            // Unicorn executes from an array view. Device hooks decode every
            // guest store as a flash command and restore the view before fetch.
            uc.mem_map(base, NOR_SIZE as u64, EngineProt::ALL)
                .map_err(|error| anyhow::anyhow!("mapping N500 NOR: {error:?}"))?;
            let bytes = uc.get_data().nor[bank].cells.clone();
            uc.mem_write(base, &bytes)
                .map_err(|error| anyhow::anyhow!("loading N500 NOR: {error:?}"))?;
            uc.add_mem_hook(
                HookType::MEM_WRITE,
                base,
                base + NOR_SIZE as u64 - 1,
                move |uc, _, address, size, value| {
                    let offset = (address - base) as usize;
                    match uc.get_data_mut().nor[bank].write(offset, size, value as u64) {
                        Ok((start, len)) => uc.get_data_mut().restore.push((bank, start, len)),
                        Err(error) => stop(uc, error),
                    }
                    false
                },
            )
            .map_err(|error| anyhow::anyhow!("wiring N500 NOR commands: {error:?}"))?;
            uc.add_mem_hook(
                HookType::MEM_READ,
                base,
                base + NOR_SIZE as u64 - 1,
                move |uc, _, address, size, _| {
                    if !uc.get_data().nor[bank].is_array() {
                        let offset = (address - base) as usize;
                        match uc.get_data().nor[bank].read(offset, size) {
                            Ok(_) => {}
                            Err(error) => stop(uc, error),
                        }
                    }
                    false
                },
            )
            .map_err(|error| anyhow::anyhow!("wiring N500 NOR reads: {error:?}"))?;
        }
        bus::map_bank(
            uc,
            u64::from(soc::BASE),
            soc::SIZE,
            Some(|uc: &mut Unicorn<'_, State>, offset: u64, size: usize| {
                let pc = uc.reg_read(RegisterARM::PC).unwrap_or(0) as u32;
                let value = match uc.get_data().soc.read(offset as u32, size) {
                    Ok(value) => value,
                    Err(error) => {
                        stop(uc, format!("{error}; PC=0x{pc:08x}"));
                        0
                    }
                };
                uc.get_data_mut().record(MmioAccess {
                    write: false,
                    address: soc::BASE + offset as u32,
                    size,
                    value,
                    pc,
                });
                value
            }),
            Some(
                |uc: &mut Unicorn<'_, State>, offset: u64, size: usize, value: u64| {
                    let pc = uc.reg_read(RegisterARM::PC).unwrap_or(0) as u32;
                    let now = uc.get_data().instructions;
                    uc.get_data_mut().record(MmioAccess {
                        write: true,
                        address: soc::BASE + offset as u32,
                        size,
                        value,
                        pc,
                    });
                    if let Err(error) = uc.get_data_mut().soc.write(offset as u32, size, value, now)
                    {
                        stop(uc, format!("{error}; PC=0x{pc:08x}"));
                    }
                },
            ),
        )
        .map_err(|error| anyhow::anyhow!("wiring N500 OneC peripherals: {error:?}"))?;
        uc.add_mem_hook(HookType::MEM_INVALID, 1, 0, |uc, kind, address, size, _| {
            let pc = uc.reg_read(RegisterARM::PC).unwrap_or(0) as u32;
            uc.get_data_mut().memory_fault = Some(MemoryFault {
                kind: format!("{kind:?}"),
                address,
                size,
                pc,
            });
            stop(
                uc,
                format!("N500 memory fault {kind:?} at 0x{address:08x}/{size}; PC=0x{pc:08x}"),
            );
            false
        })
        .map_err(|error| anyhow::anyhow!("wiring N500 memory diagnostics: {error:?}"))?;
        uc.add_intr_hook(|uc, exception| {
            if exception != 2 {
                stop(uc, format!("Unsupported N500 CPU exception {exception}"));
                return;
            }
            let cpsr = uc.reg_read(RegisterARM::CPSR).unwrap_or(0) as u32;
            let return_pc = uc.reg_read(RegisterARM::PC).unwrap_or(0);
            let status =
                (cpsr & !cpu::CPSR_MODE_MASK & !cpu::CPSR_THUMB) | 0x13 | cpu::CPSR_IRQ_DISABLE;
            let result = uc
                .reg_write(RegisterARM::CPSR, u64::from(status))
                .and_then(|_| uc.reg_write(RegisterARM::SPSR, u64::from(cpsr)))
                .and_then(|_| uc.reg_write(RegisterARM::LR, return_pc))
                .and_then(|_| uc.reg_write(RegisterARM::PC, 8));
            if let Err(error) = result {
                stop(uc, format!("entering N500 SVC: {error:?}"));
            }
            uc.get_data_mut().svcs += 1;
        })
        .map_err(|error| anyhow::anyhow!("wiring N500 SVC vector: {error:?}"))?;
        uc.add_code_hook(1, 0, |uc, address, _| {
            let restore = std::mem::take(&mut uc.get_data_mut().restore);
            for (bank, offset, size) in restore {
                let bytes = uc.get_data().nor[bank].mapped_view(offset, size);
                if let Err(error) = uc.mem_write(NOR_BASES[bank] + offset as u64, &bytes) {
                    stop(uc, format!("synchronizing N500 NOR cells: {error:?}"));
                    return;
                }
            }
            if uc.get_data().halt.is_some() {
                let _ = uc.emu_stop();
                return;
            }
            let now = uc.get_data().instructions;
            if uc
                .get_data()
                .breakpoint
                .is_some_and(|(pc, after)| address == u64::from(pc) && now >= after)
            {
                uc.get_data_mut().at_breakpoint = true;
                let _ = uc.emu_stop();
                return;
            }
            if uc.get_data().stop_on_frame && uc.get_data().soc.lcd.visible_transfers != 0 {
                uc.get_data_mut().frame_stop = true;
                let _ = uc.emu_stop();
                return;
            }
            for (bank, base) in NOR_BASES.into_iter().enumerate() {
                if (base..base + NOR_SIZE as u64).contains(&address)
                    && !uc.get_data().nor[bank].is_array()
                {
                    stop(
                        uc,
                        format!("N500 executed from NOR bank {bank} outside read-array mode"),
                    );
                    return;
                }
            }
            uc.get_data_mut().instructions += 1;
            let now = uc.get_data().instructions;
            uc.get_data_mut().soc.advance(now);
            if uc.get_data().soc.irq_pending() {
                let cpsr = uc.reg_read(RegisterARM::CPSR).unwrap_or(0) as u32;
                if cpsr & cpu::CPSR_IRQ_DISABLE == 0 {
                    if let Err(error) = cpu::enter_irq(uc, address as u32, cpsr, 0x18) {
                        stop(uc, format!("entering N500 IRQ: {error:?}"));
                    }
                    uc.get_data_mut().irqs += 1;
                }
            }
        })
        .map_err(|error| anyhow::anyhow!("wiring N500 peripheral clock: {error:?}"))?;
        uc.reg_write(RegisterARM::CPSR, 0xd3)
            .map_err(|error| anyhow::anyhow!("resetting N500: {error:?}"))?;
        uc.reg_write(RegisterARM::PC, u64::from(firmware.metadata.entry))
            .map_err(|error| anyhow::anyhow!("setting N500 reset entry: {error:?}"))?;
        Ok(machine)
    }

    fn report(&mut self, cpu_error: Option<String>) -> RunReport {
        let uc = self.cpu.raw_engine();
        let reg = |register| uc.reg_read(register).unwrap_or(0) as u32;
        let state = uc.get_data();
        let stop_reason = if let Some(error) = state.halt.clone().or(cpu_error) {
            StopReason::CpuError(error)
        } else if state.at_breakpoint || state.frame_stop {
            StopReason::Breakpoint
        } else {
            StopReason::InstructionBudget
        };
        RunReport {
            device: "n500".into(),
            execution: ExecutionReport {
                instructions: state.instructions,
                fast_forwarded_instructions: 0,
                pc: reg(RegisterARM::PC),
                cpsr: reg(RegisterARM::CPSR),
                r0: reg(RegisterARM::R0),
                r1: reg(RegisterARM::R1),
                r2: reg(RegisterARM::R2),
                r3: reg(RegisterARM::R3),
                r4: reg(RegisterARM::R4),
                r5: reg(RegisterARM::R5),
                r6: reg(RegisterARM::R6),
                r7: reg(RegisterARM::R7),
                sp: reg(RegisterARM::SP),
                lr: reg(RegisterARM::LR),
                stop_reason,
                memory_fault: state.memory_fault.clone(),
                recent_mmio: state.recent_mmio.iter().cloned().collect(),
            },
            lcd_frames: state.soc.lcd.transfers,
            details: serde_json::json!({
                "lcd_commands": state.soc.lcd.commands, "lcd_data_writes": state.soc.lcd.writes,
                "lcd_enabled": state.soc.lcd.enabled(), "lcd_visible_frames": state.soc.lcd.visible_transfers,
                "lcd_has_pixels": state.soc.lcd.has_pixels(), "stopped_on_frame": state.frame_stop,
                "irqs": state.irqs, "svcs": state.svcs, "mmio_trace": state.trace,
                "system_clock_ticks": state.soc.system_ticks,
                "timing": "Instruction-based quarter-bit clock approximation; ARM clock unverified",
                "unimplemented": ["Radio/frame interrupt source and OneC DSP execution", "SIM", "Key scanner contacts", "LCD annunciator segment mapping"],
                "reset_transfers": state.soc.reset_transfers, "serial_transfers": state.soc.serial_transfers,
                "eeprom_transactions": state.soc.i2c.transactions, "eeprom_reads": state.soc.i2c.reads,
                "eeprom_writes": state.soc.i2c.writes,
                "nor_programs": state.nor.iter().map(|nor| nor.programs).sum::<u64>(),
                "nor_erases": state.nor.iter().map(|nor| nor.erases).sum::<u64>(),
                "nor_commands": state.nor.iter().map(|nor| nor.commands).sum::<u64>(),
            }),
        }
    }
}

impl DeviceRuntime for N500Machine {
    fn run(&mut self, budget: u64) -> Result<RunReport> {
        ensure!(budget != 0, "N500 instruction budget must be nonzero");
        let uc = self.cpu.raw_engine();
        if uc.get_data().halt.is_some() || uc.get_data().frame_stop {
            return Ok(self.report(None));
        }
        uc.get_data_mut().at_breakpoint = false;
        let pc = uc
            .reg_read(RegisterARM::PC)
            .map_err(|error| anyhow::anyhow!("reading N500 PC: {error:?}"))?;
        let cpsr = uc
            .reg_read(RegisterARM::CPSR)
            .map_err(|error| anyhow::anyhow!("reading N500 CPSR: {error:?}"))?;
        let start = pc | u64::from(cpsr & u64::from(cpu::CPSR_THUMB) != 0);
        let result = uc.emu_start(
            start,
            0xffff_ffff,
            0,
            budget.try_into().unwrap_or(usize::MAX),
        );
        Ok(self.report(result.err().map(|error| format!("{error:?}"))))
    }
    fn frame(&mut self) -> Option<Frame> {
        self.cpu.raw_engine().get_data().soc.lcd.frame()
    }
    fn frame_revision(&mut self) -> u64 {
        let lcd = &self.cpu.raw_engine().get_data().soc.lcd;
        lcd.writes + lcd.commands
    }
    fn set_phone_keys(&mut self, keys: u32) -> Result<()> {
        ensure!(
            keys == 0,
            "N500 key scanner contacts have not yet been implemented"
        );
        Ok(())
    }
    fn set_breakpoint(&mut self, pc: u32, after: u64) {
        self.cpu.raw_engine().get_data_mut().breakpoint = Some((pc & !1, after));
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::firmware::{n500::AxfLoader, FirmwareInput, FirmwareLoader};
    use std::collections::BTreeMap;

    fn firmware(words: &[(usize, &[u32])]) -> LoadedFirmware {
        // Small original ARM test program, independent of proprietary firmware.
        let mut image = vec![0; 0x200];
        for offset in [0, 4, 8, 12, 16, 24, 28] {
            let target = if offset == 24 { 0x100 } else { 0x40 };
            let branch = 0xea00_0000 | ((target - offset - 8) / 4) as u32;
            image[offset..offset + 4].copy_from_slice(&branch.to_le_bytes());
        }
        for &(address, block) in words {
            for (index, word) in block.iter().enumerate() {
                let offset = address + index * 4;
                image[offset..offset + 4].copy_from_slice(&word.to_le_bytes());
            }
        }
        AxfLoader
            .load(FirmwareInput {
                image,
                companions: BTreeMap::new(),
            })
            .unwrap()
    }

    #[test]
    fn reset_program_draws_through_the_profile_bus_and_unknown_mmio_stops() {
        let firmware = firmware(&[(
            0x40,
            &[
                0xe3a0_0301, // mov r0,#0x04000000
                0xe280_0c03, // add r0,r0,#0x300
                0xe3a0_10af,
                0xe5c0_1000, // display on
                0xe3a0_1004,
                0xe5c0_1000, // column four
                0xe3a0_1001,
                0xe5c0_1001, // guest pixel
                0xe280_0c7d, // add r0,r0,#0x7d00 -> unknown 0x04008000
                0xe1d0_10b0, // ldrh r1,[r0]
            ],
        )]);
        let mut machine = N500Machine::new(firmware, RuntimeOptions::default()).unwrap();
        let report = machine.run(100).unwrap();
        assert!(
            matches!(report.execution.stop_reason, StopReason::CpuError(ref error)
            if error.contains("0x04008000") && error.contains("PC=0x00000064"))
        );
        let frame = machine.frame().unwrap();
        frame.validate().unwrap();
        assert_eq!((frame.width, frame.height), (128, 64));
        assert_eq!(&frame.rgb[..3], &[0, 0, 0]);
        assert_eq!(&frame.rgb[3..6], &[255, 255, 255]);
        assert_eq!(
            machine.run(100).unwrap().execution.instructions,
            report.execution.instructions
        );
    }

    #[test]
    fn clock_irq_zero_ack_is_safe_with_nested_irqs_and_across_run_boundaries() {
        let firmware = firmware(&[
            (
                0x40,
                &[
                    0xe321_f0d2,
                    0xe59f_d038, // IRQ mode, SP=0x01007000
                    0xe321_f0d3,
                    0xe59f_d034, // SVC mode, SP=0x01006000
                    0xe3a0_0301,
                    0xe280_0a01, // r0=OneC timer bank
                    0xe3a0_1003,
                    0xe1c0_10b0, // enable frame counter/RTOS clock
                    0xe321_f053, // unmask architectural IRQ
                    0xeaff_fffe, // idle loop
                ],
            ),
            (0x84, &[0x0100_7000, 0x0100_6000]),
            (
                0x100,
                &[
                    0xe92d_4007, // push {r0-r2,lr}
                    0xe3a0_0301,
                    0xe280_0a01,
                    0xe3a0_1000,
                    0xe1c0_10b6, // any write acknowledges system IRQ
                    0xe321_f052, // allow nested IRQs, as the original dispatch does
                    0xe3a0_0401, // r0=RAM
                    0xe590_1000,
                    0xe281_1001,
                    0xe580_1000,
                    0xe321_f0d2,
                    0xe8bd_4007,
                    0xe25e_f004, // restore registers; subs pc,lr,#4
                ],
            ),
        ]);
        let mut machine = N500Machine::new(firmware, RuntimeOptions::default()).unwrap();
        for _ in 0..2 {
            let report = machine.run(250_000).unwrap();
            assert!(matches!(
                report.execution.stop_reason,
                StopReason::InstructionBudget
            ));
            assert!(report.execution.memory_fault.is_none());
            assert_eq!(report.execution.sp, 0x0100_6000);
        }
        let uc = machine.cpu.raw_engine();
        assert_eq!(uc.get_data().irqs, 2);
        assert_eq!(uc.mem_read_as_vec(RAM_BASE, 4).unwrap(), 2u32.to_le_bytes());
    }
}
