//! Configured ARM execution and architectural coprocessor/IRQ support.
use unicorn_engine::unicorn_const::{uc_error, Arch};
pub use unicorn_engine::unicorn_const::{ArmCpuModel, HookType, Mode, Prot as EngineProt};
pub use unicorn_engine::{RegisterARM, Unicorn};

/// A device supplies the backend model, execution mode and optional register setup.
/// `None` preserves the model selected by Unicorn's mode flags.
#[derive(Clone, Copy)]
pub struct CpuConfig {
    pub mode: Mode,
    pub model: Option<ArmCpuModel>,
    pub initial_registers: &'static [(RegisterARM, u64)],
}

pub struct ArmCpu<D: 'static> {
    engine: Unicorn<'static, D>,
}

pub const CPSR_MODE_MASK: u32 = 0x1f;
pub const CPSR_MODE_IRQ: u32 = 0x12;
pub const CPSR_IRQ_DISABLE: u32 = 1 << 7;
pub const CPSR_THUMB: u32 = 1 << 5;

#[derive(Default)]
pub struct TcmRegisters {
    regions: [u32; 2],
    identification: u32,
}

impl TcmRegisters {
    pub fn new(identification: u32) -> Self {
        Self {
            regions: [0; 2],
            identification,
        }
    }

    /// Recognize ARM MRC/MCR instructions for TCM identification and control.
    /// No firmware addresses or replacement guest instructions are involved.
    pub fn recognizes(instruction: u32) -> bool {
        let operation = instruction & 0x0fff_0fff;
        operation == 0x0e10_0f50 // MRC p15,0,Rd,c0,c0,2 (TCMTR)
            || matches!(operation, 0x0e09_0f11 | 0x0e09_0f31 | 0x0e19_0f11 | 0x0e19_0f31)
    }

    pub fn execute(&mut self, instruction: u32, value: u32) -> Option<u32> {
        let operation = instruction & 0x0fff_0fff;
        if operation == 0x0e10_0f50 {
            return Some(self.identification);
        }
        let index = ((instruction >> 5) & 1) as usize;
        if instruction & (1 << 20) != 0 {
            Some(self.regions[index])
        } else {
            self.regions[index] = value;
            None
        }
    }
}

pub fn arm_condition_passed(condition: u32, cpsr: u32) -> bool {
    let n = cpsr & (1 << 31) != 0;
    let z = cpsr & (1 << 30) != 0;
    let c = cpsr & (1 << 29) != 0;
    let v = cpsr & (1 << 28) != 0;
    match condition {
        0 => z,
        1 => !z,
        2 => c,
        3 => !c,
        4 => n,
        5 => !n,
        6 => v,
        7 => !v,
        8 => c && !z,
        9 => !c || z,
        10 => n == v,
        11 => n != v,
        12 => !z && n == v,
        13 => z || n != v,
        14 => true,
        _ => false,
    }
}

impl<D: 'static> ArmCpu<D> {
    pub fn new_with_data(config: CpuConfig, data: D) -> Result<Self, uc_error> {
        let mut engine = Unicorn::new_with_data(Arch::ARM, config.mode, data)?;
        if let Some(model) = config.model {
            engine.ctl_set_cpu_model(model as i32)?;
        }
        // Optional coprocessor setup may be unsupported by a backend model.
        for &(register, value) in config.initial_registers {
            let _ = engine.reg_write(register, value);
        }
        Ok(Self { engine })
    }

    pub fn raw_engine(&mut self) -> &mut Unicorn<'static, D> {
        &mut self.engine
    }
}

pub trait TcmAccess {
    fn tcm_registers(&mut self) -> &mut TcmRegisters;
    fn skip_tcm_instruction(&self) -> bool {
        false
    }
}

/// Take a normal ARM IRQ through a vector address supplied by the board.
pub fn enter_irq<D>(
    cpu: &mut Unicorn<'_, D>,
    pc: u32,
    cpsr: u32,
    vector: u64,
) -> Result<(), uc_error> {
    let status = (cpsr & !CPSR_MODE_MASK & !CPSR_THUMB) | CPSR_MODE_IRQ | CPSR_IRQ_DISABLE;
    cpu.reg_write(RegisterARM::CPSR, u64::from(status))?;
    cpu.reg_write(RegisterARM::SPSR, u64::from(cpsr))?;
    cpu.reg_write(RegisterARM::LR, u64::from(pc).wrapping_add(4))?;
    cpu.reg_write(RegisterARM::PC, vector)
}

pub fn install_tcm_support<D: TcmAccess + 'static>(
    cpu: &mut Unicorn<'static, D>,
    image: &[u8],
    aliases: &[u64],
) -> Result<(), uc_error> {
    for (index, bytes) in image.chunks_exact(4).enumerate() {
        let instruction = u32::from_le_bytes(bytes.try_into().expect("ARM instruction word"));
        if !TcmRegisters::recognizes(instruction) {
            continue;
        }
        let offset = index as u64 * 4;
        for address in aliases.iter().map(|base| base + offset) {
            cpu.add_code_hook(address, address, |uc, pc, size| {
                let cpsr = uc.reg_read(RegisterARM::CPSR).unwrap_or(0) as u32;
                if size != 4 || cpsr & CPSR_THUMB != 0 || uc.get_data().skip_tcm_instruction() {
                    return;
                }
                let mut bytes = [0; 4];
                if uc.mem_read(pc, &mut bytes).is_err() {
                    return;
                }
                let instruction = u32::from_le_bytes(bytes);
                if !TcmRegisters::recognizes(instruction)
                    || !arm_condition_passed(instruction >> 28, cpsr)
                {
                    return;
                }
                let rd = (instruction >> 12) & 15;
                let register = [
                    RegisterARM::R0,
                    RegisterARM::R1,
                    RegisterARM::R2,
                    RegisterARM::R3,
                    RegisterARM::R4,
                    RegisterARM::R5,
                    RegisterARM::R6,
                    RegisterARM::R7,
                    RegisterARM::R8,
                    RegisterARM::R9,
                    RegisterARM::R10,
                    RegisterARM::R11,
                    RegisterARM::R12,
                    RegisterARM::SP,
                    RegisterARM::LR,
                    RegisterARM::PC,
                ][rd as usize];
                let input = if rd == 15 {
                    pc as u32 + 8
                } else {
                    uc.reg_read(register).unwrap_or(0) as u32
                };
                if let Some(output) = uc
                    .get_data_mut()
                    .tcm_registers()
                    .execute(instruction, input)
                {
                    if rd == 15 {
                        let _ = uc.reg_write(
                            RegisterARM::CPSR,
                            u64::from((cpsr & 0x0fff_ffff) | (output & 0xf000_0000)),
                        );
                    } else {
                        let _ = uc.reg_write(register, u64::from(output));
                    }
                }
                // Keep Unicorn's architectural execution for all other ARM
                // instructions. These TCM registers are a board extension.
                let _ = uc.reg_write(RegisterARM::PC, pc + 4);
            })?;
        }
    }
    Ok(())
}
