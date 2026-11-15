//! OneC board allocations inferred from startup code, linker map and EEPROM part.
//! These modeled sizes are not a claim about the fitted memory package capacity.
pub const RAM_BASE: u64 = 0x0100_0000;
pub const RAM_SIZE: u64 = 0x8_0000;
pub const NOR_BASES: [u64; 2] = [0, 0x0080_0000];
pub const NOR_SIZE: usize = 0x20_0000;
pub const EEPROM_SIZE: usize = 0x8000;
