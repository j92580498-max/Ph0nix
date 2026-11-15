//! Architecture-independent execution stop and memory/bus diagnostics.
use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum StopReason {
    InstructionBudget,
    Breakpoint,
    CpuError(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MemoryFault {
    pub kind: String,
    pub address: u64,
    pub size: usize,
    pub pc: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MmioAccess {
    pub write: bool,
    pub address: u32,
    pub size: usize,
    pub value: u64,
    pub pc: u32,
}

/// Common CPU/bus report used by every device runtime.
#[derive(Debug, Clone, Serialize)]
pub struct ExecutionReport {
    pub instructions: u64,
    pub fast_forwarded_instructions: u64,
    pub pc: u32,
    pub cpsr: u32,
    pub r0: u32,
    pub r1: u32,
    pub r2: u32,
    pub r3: u32,
    pub r4: u32,
    pub r5: u32,
    pub r6: u32,
    pub r7: u32,
    pub sp: u32,
    pub lr: u32,
    pub stop_reason: StopReason,
    pub memory_fault: Option<MemoryFault>,
    pub recent_mmio: Vec<MmioAccess>,
}
