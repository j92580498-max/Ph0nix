//! Phone firmware emulator with separate core, device and firmware layers.
extern crate self as e830;

pub mod core;
pub mod devices;
pub mod firmware;
pub mod runner;
mod profile;
pub use profile::ProfileConfig;

// Compatibility exports for existing E830 frontends and diagnostic clients.
pub use firmware::e830::{FirmwareError, FirmwareSet};
pub use devices::e830::{BootReport, E830Machine, MachineError, MemoryFault,
    MmioAccess, StopReason, FLASH_BASE, RAM_BASE, RAM_SIZE, E830_TFS_SOURCE_BASE,
    LCD_WIDTH, LCD_HEIGHT};
pub use devices::keys::PhoneKey;
