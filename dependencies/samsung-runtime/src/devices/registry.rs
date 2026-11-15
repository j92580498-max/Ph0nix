//! Registration belongs to the device layer; adding a plugin never edits core.
use super::runtime::{DeviceInfo, DeviceProfile, DeviceRuntime, Frame, RunReport, RuntimeOptions};
use crate::firmware::{FirmwareFiles, FirmwareLoader, LoadedFirmware};
use anyhow::{ensure, Context, Result};
use std::collections::BTreeMap;

#[derive(Default)]
pub struct Registry {
    devices: BTreeMap<String, Box<dyn DeviceProfile>>,
    loaders: BTreeMap<String, Box<dyn FirmwareLoader>>,
}

impl Registry {
    pub fn builtin() -> Self {
        let mut registry = Self::default();
        registry
            .register_loader(Box::new(crate::firmware::e830::ClaLoader))
            .unwrap();
        registry
            .register_device(Box::new(super::e830::E830Profile))
            .unwrap();
        registry
            .register_loader(Box::new(crate::firmware::n500::AxfLoader))
            .unwrap();
        registry
            .register_device(Box::new(super::n500::N500Profile))
            .unwrap();
        registry
    }

    pub fn register_loader(&mut self, loader: Box<dyn FirmwareLoader>) -> Result<()> {
        let id = loader.id().to_owned();
        ensure!(
            !self.loaders.contains_key(&id),
            "Firmware loader {id} is already registered"
        );
        self.loaders.insert(id, loader);
        Ok(())
    }

    pub fn register_device(&mut self, device: Box<dyn DeviceProfile>) -> Result<()> {
        let info = device.info();
        ensure!(
            !self.devices.contains_key(info.id),
            "Device profile {} is already registered",
            info.id
        );
        ensure!(
            self.loaders.contains_key(info.default_loader),
            "Register firmware loader {} first",
            info.default_loader
        );
        self.devices.insert(info.id.to_owned(), device);
        Ok(())
    }

    pub fn devices(&self) -> Vec<DeviceInfo> {
        self.devices
            .values()
            .map(|profile| profile.info())
            .collect()
    }

    pub fn device(&self, id: &str) -> Result<&dyn DeviceProfile> {
        self.devices
            .get(id)
            .map(Box::as_ref)
            .with_context(|| format!("Unknown device profile {id}"))
    }

    pub fn loader(&self, id: &str) -> Result<&dyn FirmwareLoader> {
        self.loaders
            .get(id)
            .map(Box::as_ref)
            .with_context(|| format!("Unknown firmware loader {id}"))
    }

    pub fn load(
        &self,
        device: &str,
        loader: Option<&str>,
        files: &FirmwareFiles,
    ) -> Result<LoadedFirmware> {
        let info = self.device(device)?.info();
        let loader = self.loader(loader.unwrap_or(info.default_loader))?;
        let loaded = loader.load(loader.prepare_files(files)?.read()?)?;
        loaded.validate()?;
        Ok(loaded)
    }

    pub fn open(
        &self,
        device: &str,
        loader: Option<&str>,
        files: &FirmwareFiles,
        options: RuntimeOptions,
    ) -> Result<Machine> {
        let profile = self.device(device)?;
        let loaded = self.load(device, loader, files)?;
        Ok(Machine {
            info: profile.info(),
            runtime: profile.create(loaded, options)?,
        })
    }
}

pub struct Machine {
    pub info: DeviceInfo,
    runtime: Box<dyn DeviceRuntime>,
}

impl Machine {
    pub fn run(&mut self, budget: u64) -> Result<RunReport> {
        let mut report = self.runtime.run(budget)?;
        report.device = self.info.id.into();
        Ok(report)
    }
    pub fn frame(&mut self) -> Option<Frame> {
        self.runtime.frame()
    }
    pub fn frame_revision(&mut self) -> u64 {
        self.runtime.frame_revision()
    }
    pub fn set_phone_keys(&mut self, keys: u32) -> Result<()> {
        self.runtime.set_phone_keys(keys)
    }
    pub fn set_breakpoint(&mut self, pc: u32, after: u64) {
        self.runtime.set_breakpoint(pc, after);
    }
    pub fn runtime_mut(&mut self) -> &mut dyn DeviceRuntime {
        self.runtime.as_mut()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::firmware::{FileRole, FirmwareInput};

    // A third-party profile can reuse a board and supply a separate loader.
    // This test exercises registration and execution without proprietary files.
    struct PluginLoader;
    impl FirmwareLoader for PluginLoader {
        fn id(&self) -> &'static str {
            "test-flat-image"
        }
        fn roles(&self) -> Vec<FileRole> {
            crate::firmware::n500::AxfLoader.roles()
        }
        fn load(&self, input: FirmwareInput) -> Result<LoadedFirmware> {
            let mut firmware = crate::firmware::n500::AxfLoader.load(input)?;
            firmware.metadata.loader = self.id().into();
            Ok(firmware)
        }
    }

    struct PluginProfile;
    impl DeviceProfile for PluginProfile {
        fn info(&self) -> DeviceInfo {
            DeviceInfo {
                id: "test-phone",
                name: "Test phone",
                default_loader: "test-flat-image",
                ..super::super::n500::N500Profile.info()
            }
        }
        fn create(
            &self,
            firmware: LoadedFirmware,
            options: RuntimeOptions,
        ) -> Result<Box<dyn DeviceRuntime>> {
            super::super::n500::N500Profile.create(firmware, options)
        }
    }

    #[test]
    fn external_profile_and_loader_register_and_execute_without_core_changes() {
        let mut registry = Registry::builtin();
        assert!(registry.register_device(Box::new(PluginProfile)).is_err());
        registry.register_loader(Box::new(PluginLoader)).unwrap();
        assert!(registry.register_loader(Box::new(PluginLoader)).is_err());
        registry.register_device(Box::new(PluginProfile)).unwrap();
        assert!(registry.register_device(Box::new(PluginProfile)).is_err());
        assert_eq!(registry.devices().len(), 3);

        struct Directory(std::path::PathBuf);
        impl Drop for Directory {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let directory = Directory(std::env::temp_dir().join(format!(
            "phone-plugin-test-{}-{}", std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        )));
        std::fs::create_dir(&directory.0).unwrap();
        let mut bytes = vec![0; 0x100];
        for offset in [0, 4, 8, 12, 16, 24, 28] {
            let branch = 0xea00_0000u32 | ((0x40 - offset - 8) / 4) as u32;
            bytes[offset..offset + 4].copy_from_slice(&branch.to_le_bytes());
        }
        bytes[0x40..0x44].copy_from_slice(&0xeaff_fffeu32.to_le_bytes());
        let image = directory.0.join("test.axf");
        std::fs::write(&image, bytes).unwrap();
        let files = registry
            .loader("test-flat-image")
            .unwrap()
            .classify(vec![image])
            .unwrap();
        let loaded = registry.load("test-phone", None, &files).unwrap();
        assert_eq!(loaded.metadata.loader, "test-flat-image");
        let mut machine = registry
            .open("test-phone", None, &files, RuntimeOptions::default())
            .unwrap();
        assert_eq!(machine.info.id, "test-phone");
        let report = machine.run(100).unwrap();
        assert_eq!(report.device, "test-phone");
        assert_eq!(report.execution.instructions, 100);
        assert!(report.execution.memory_fault.is_none());
        assert!(matches!(
            report.execution.stop_reason,
            crate::StopReason::InstructionBudget
        ));
    }
}
