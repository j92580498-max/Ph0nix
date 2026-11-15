//! The original E830 board, preserved as the first device profile.
mod board;
mod memory;
mod constants;
mod profile;
pub mod frontend;
use profile::CPU_CONFIG;
pub use profile::E830Profile;
pub use board::{BootReport, E830Machine, MachineError, MemoryFault, MmioAccess, StopReason, LCD_HEIGHT, LCD_WIDTH};
pub use memory::{FLASH_BASE, RAM_BASE, RAM_SIZE, E830_TFS_SOURCE_BASE, validate_nand_image};
pub(crate) mod keypad;
pub(crate) mod i2c;
pub(crate) mod nor;
pub(crate) mod onenand;
pub(crate) mod serial;
pub(crate) mod sim;
pub(crate) mod uart;
