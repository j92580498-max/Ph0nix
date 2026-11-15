//! Samsung SGH-N500 / Philips OneC board. See the investigation for evidence limits.
mod board;
pub mod display;
mod eeprom;
pub mod memory;
mod nor;
mod soc;

use super::runtime::{DeviceInfo, DeviceProfile, DeviceRuntime, DisplayInfo, RuntimeOptions};
use crate::firmware::{n500::FORMAT, LoadedFirmware};
use anyhow::{ensure, Result};

pub use board::N500Machine;

pub struct N500Profile;

impl DeviceProfile for N500Profile {
    fn info(&self) -> DeviceInfo {
        DeviceInfo {
            id: "n500",
            name: "Samsung SGH-N500",
            default_loader: "n500-axf",
            display: Some(DisplayInfo {
                width: display::WIDTH,
                height: display::HEIGHT,
                format: "monochrome",
            }),
            support: "Experimental firmware boot graphics; radio/DSP and keys incomplete",
            has_nand: false,
            has_keys: false,
            storage_hint: None,
        }
    }

    fn create(
        &self,
        firmware: LoadedFirmware,
        options: RuntimeOptions,
    ) -> Result<Box<dyn DeviceRuntime>> {
        ensure!(
            firmware.metadata.format == FORMAT,
            "N500 requires an N500 flat AXF loader"
        );
        Ok(Box::new(N500Machine::new(firmware, options)?))
    }
}
