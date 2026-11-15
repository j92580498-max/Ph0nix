//! Image formats, firmware revision adapters, HLE and diagnostic tools.
pub mod e830;
pub mod n500;

use crate::core::memory::Permissions;
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Host paths are independent of a handset's file formats or companion roles.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FirmwareFiles {
    pub image: PathBuf,
    #[serde(default)]
    pub companions: BTreeMap<String, PathBuf>,
}

impl FirmwareFiles {
    pub fn read(&self) -> Result<FirmwareInput> {
        let image = std::fs::read(&self.image)
            .with_context(|| format!("reading firmware image {}", self.image.display()))?;
        let companions = self
            .companions
            .iter()
            .map(|(role, path)| {
                std::fs::read(path)
                    .with_context(|| format!("reading {role} {}", path.display()))
                    .map(|bytes| (role.clone(), bytes))
            })
            .collect::<Result<_>>()?;
        Ok(FirmwareInput { image, companions })
    }
}

pub struct FirmwareInput {
    pub image: Vec<u8>,
    pub companions: BTreeMap<String, Vec<u8>>,
}

impl FirmwareInput {
    pub fn image(&self, role: &str) -> Option<&[u8]> {
        if role == "image" {
            Some(&self.image)
        } else {
            self.companions.get(role).map(Vec::as_slice)
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct FileRole {
    pub role: &'static str,
    pub extensions: &'static [&'static str],
    pub required: bool,
}

/// A segment describes bytes and a load address, never a firmware-specific core hook.
#[derive(Clone, Debug, Serialize)]
pub struct ImageSegment {
    pub name: String,
    pub role: String,
    pub offset: usize,
    pub size: usize,
    pub address: u64,
    pub permissions: Permissions,
}

#[derive(Clone, Debug, Serialize)]
pub struct FirmwareMetadata {
    pub loader: String,
    pub format: String,
    pub revision: Option<String>,
    pub image_sha256: String,
    pub image_size: usize,
    pub entry: u32,
    pub instruction_set: String,
    pub segments: Vec<ImageSegment>,
    pub facts: BTreeMap<String, String>,
    pub limitations: Vec<String>,
}

pub struct LoadedFirmware {
    pub metadata: FirmwareMetadata,
    pub input: FirmwareInput,
}

impl LoadedFirmware {
    pub fn segment(&self, segment: &ImageSegment) -> Result<&[u8]> {
        self.input
            .image(&segment.role)
            .and_then(|bytes| {
                segment
                    .offset
                    .checked_add(segment.size)
                    .and_then(|end| bytes.get(segment.offset..end))
            })
            .with_context(|| format!("{} references bytes outside {}", segment.name, segment.role))
    }

    pub fn validate(&self) -> Result<()> {
        for segment in &self.metadata.segments {
            self.segment(segment)?;
            ensure!(segment.size != 0, "{} is empty", segment.name);
            ensure!(
                segment
                    .address
                    .checked_add(segment.size as u64)
                    .is_some_and(|end| end <= 1u64 << 32),
                "{} exceeds the address space",
                segment.name
            );
        }
        Ok(())
    }
}

pub trait FirmwareLoader: Send + Sync {
    fn id(&self) -> &'static str;
    fn roles(&self) -> Vec<FileRole>;
    fn load(&self, input: FirmwareInput) -> Result<LoadedFirmware>;

    /// Optional host discovery and companion rules belong to the image loader.
    fn bundled(&self, _root: &Path) -> Option<FirmwareFiles> {
        None
    }
    fn prepare_files(&self, files: &FirmwareFiles) -> Result<FirmwareFiles> {
        Ok(files.clone())
    }
    fn selection_hint(&self) -> &'static str {
        "Select the primary image and its matching companion files."
    }

    fn classify(&self, paths: Vec<PathBuf>) -> Result<FirmwareFiles> {
        let roles = self.roles();
        let mut selected = BTreeMap::new();
        for path in paths {
            let extension = path
                .extension()
                .and_then(|value| value.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            let role = roles
                .iter()
                .find(|role| role.extensions.contains(&extension.as_str()))
                .with_context(|| format!("{} does not accept {}", self.id(), path.display()))?;
            ensure!(
                selected.insert(role.role.to_owned(), path).is_none(),
                "Select only one {} file",
                role.role
            );
        }
        for role in roles.iter().filter(|role| role.required) {
            ensure!(
                selected.contains_key(role.role),
                "The {} file is required",
                role.role
            );
        }
        let image = selected
            .remove("image")
            .context("The primary firmware image is required")?;
        Ok(FirmwareFiles {
            image,
            companions: selected,
        })
    }
}

pub fn image_sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
