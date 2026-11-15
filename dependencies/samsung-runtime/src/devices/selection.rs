//! Profile-neutral host selection. Legacy E830 settings migrate on deserialization.
use super::registry::Registry;
use crate::firmware::{e830::FirmwarePaths, FirmwareFiles};
use anyhow::Result;
use serde::{Deserialize, Deserializer, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Serialize)]
pub struct FirmwareSelection {
    pub device: String,
    pub loader: Option<String>,
    pub files: FirmwareFiles,
}

impl<'de> Deserialize<'de> for FirmwareSelection {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Stored {
            Profile {
                device: String,
                #[serde(default)]
                loader: Option<String>,
                files: FirmwareFiles,
            },
            Legacy(FirmwarePaths),
        }
        Ok(match Stored::deserialize(deserializer)? {
            Stored::Profile {
                device,
                loader,
                files,
            } => Self {
                device,
                loader,
                files,
            },
            Stored::Legacy(paths) => paths.into(),
        })
    }
}

impl From<FirmwarePaths> for FirmwareSelection {
    fn from(paths: FirmwarePaths) -> Self {
        let mut companions = std::collections::BTreeMap::from([("eeprom".into(), paths.eeprom)]);
        if let Some(tfs) = paths.tfs {
            companions.insert("tfs_package".into(), tfs);
        }
        if let Some(nand) = paths.onenand {
            companions.insert("nand".into(), nand);
        }
        Self {
            device: "e830".into(),
            loader: None,
            files: FirmwareFiles {
                image: paths.cla,
                companions,
            },
        }
    }
}

impl FirmwareSelection {
    pub fn classify(registry: &Registry, device: &str, paths: Vec<PathBuf>) -> Result<Self> {
        let info = registry.device(device)?.info();
        Ok(Self {
            device: device.into(),
            loader: None,
            files: registry.loader(info.default_loader)?.classify(paths)?,
        })
    }

    pub fn bundled(registry: &Registry, root: &Path) -> Option<Self> {
        registry.devices().into_iter().find_map(|info| {
            registry
                .loader(info.default_loader)
                .ok()?
                .bundled(root)
                .map(|files| Self {
                    device: info.id.into(),
                    loader: None,
                    files,
                })
        })
    }

    pub fn prepared_files(&self, registry: &Registry) -> Result<FirmwareFiles> {
        let info = registry.device(&self.device)?.info();
        registry
            .loader(self.loader.as_deref().unwrap_or(info.default_loader))?
            .prepare_files(&self.files)
    }
}
