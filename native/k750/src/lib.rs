// SPDX-License-Identifier: GPL-2.0-or-later
//! Portable, guarded DB2010 profile. Guest bytes are never patched by this crate.
pub mod board;
pub mod catalog;
pub mod keys;
pub mod loader;
mod registers;
pub mod worker;

use anyhow::{ensure, Result};
use e830::devices::{registry::Registry, runtime::*};
use e830::firmware::LoadedFirmware;

pub fn registry() -> Registry {
    let mut registry = Registry::default();
    registry.register_loader(Box::new(loader::BabeLoader)).expect("unique BABE loader");
    registry.register_device(Box::new(K750Profile)).expect("unique K750i profile");
    registry
}

pub struct K750Profile;

impl DeviceProfile for K750Profile {
    fn runtime_notice(&self) -> RuntimeNotice {
        RuntimeNotice {
            label: "K750i R1DB001 CID49 · experimental compatibility mode".into(),
            detail: "Original firmware draws the phone UI. Import matching MAIN and FS; first boot can take several minutes. Security, SIM, audio and calibration services include synthetic assumptions. No cellular network is available.".into(),
            experimental: true,
        }
    }
    fn info(&self) -> DeviceInfo {
        DeviceInfo {
            id: "k750i",
            name: "Sony Ericsson K750i",
            default_loader: "db2010-babe",
            display: Some(DisplayInfo { width: 176, height: 220, format: "rgb565" }),
            support: "R1DB001 CID49 native QEMU startup and Settings; compatibility inputs and synthetic services",
            has_nand: false,
            has_keys: true,
            storage_hint: Some("Audited MAIN .mbn and FS .fbn; native apps include the declared startup compatibility inputs"),
        }
    }

    fn create(&self, firmware: LoadedFirmware, options: RuntimeOptions) -> Result<Box<dyn DeviceRuntime>> {
        ensure!(firmware.metadata.format == loader::FORMAT, "K750i requires its audited DB2010 BABE loader");
        Ok(Box::new(board::K750Machine::new(firmware, options)?))
    }
}

