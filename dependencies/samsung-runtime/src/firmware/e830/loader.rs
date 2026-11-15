use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::devices::e830::FLASH_BASE;
use crate::core::memory::Permissions;
use crate::firmware::{FileRole, FirmwareInput, FirmwareLoader, FirmwareMetadata,
    ImageSegment, LoadedFirmware, image_sha256};

const RESET_VECTOR_OFFSET: usize = 0x15c;
const RESET_VECTOR_INSTRUCTION: [u8; 4] = [0x54, 0xf1, 0x9f, 0xe5];
const NSF_HEADER_MAGIC: &[u8; 3] = b"NF4";
const NSF_HEADER_SIZE: usize = 20;

#[derive(Debug, Error)]
pub enum FirmwareError {
    #[error("could not read firmware file {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("CLA image is too short to contain the E830 reset vector")]
    ClaTooShort,
    #[error("CLA image does not start with the E830 ARM reset-vector instruction")]
    UnsupportedCla,
    #[error(
        "CLA reset handler 0x{target:08x} is outside the mapped image 0x{start:08x}..0x{end:08x}"
    )]
    ResetHandlerOutsideImage { target: u32, start: u32, end: u64 },
    #[error("EEPROM image is empty")]
    EmptyEeprom,
    #[error("TFS image is empty")]
    EmptyTfs,
    #[error("TFS installer package is empty")]
    EmptyTfsPackage,
}

#[derive(Debug)]
pub struct FirmwareSet {
    cla: Vec<u8>,
    eeprom: Vec<u8>,
    tfs_image: Option<Vec<u8>>,
    tfs_package: Option<Vec<u8>>,
    onenand_image: Option<Vec<u8>>,
    reset_handler: u32,
}

impl FirmwareSet {
    pub fn load(
        cla_path: &Path,
        eeprom_path: &Path,
        tfs_image_path: Option<&Path>,
        tfs_package_path: Option<&Path>,
    ) -> Result<Self, FirmwareError> {
        let cla = read_file(cla_path)?;
        let eeprom = read_file(eeprom_path)?;
        let tfs_image = tfs_image_path.map(read_file).transpose()?;
        let tfs_package = tfs_package_path.map(read_file).transpose()?;
        Self::from_bytes(cla, eeprom, tfs_image, tfs_package)
    }

    pub fn from_bytes(
        cla: Vec<u8>,
        eeprom: Vec<u8>,
        tfs_image: Option<Vec<u8>>,
        tfs_package: Option<Vec<u8>>,
    ) -> Result<Self, FirmwareError> {
        let reset_handler = validate_cla(&cla)?;
        if eeprom_payload_offset(&eeprom) >= eeprom.len() {
            return Err(FirmwareError::EmptyEeprom);
        }
        if tfs_image.as_ref().is_some_and(Vec::is_empty) {
            return Err(FirmwareError::EmptyTfs);
        }
        if tfs_package.as_ref().is_some_and(Vec::is_empty) {
            return Err(FirmwareError::EmptyTfsPackage);
        }
        Ok(Self {
            cla,
            eeprom,
            tfs_image,
            tfs_package,
            onenand_image: None,
            reset_handler,
        })
    }

    pub fn cla(&self) -> &[u8] {
        &self.cla
    }

    pub fn eeprom(&self) -> &[u8] {
        &self.eeprom
    }

    pub fn eeprom_payload(&self) -> &[u8] {
        &self.eeprom[eeprom_payload_offset(&self.eeprom)..]
    }

    pub fn tfs_image(&self) -> Option<&[u8]> {
        self.tfs_image.as_deref()
    }

    pub fn tfs_package(&self) -> Option<&[u8]> {
        self.tfs_package.as_deref()
    }

    pub fn with_onenand_image(mut self, path: &Path) -> Result<Self, FirmwareError> {
        self.onenand_image = Some(read_file(path)?);
        Ok(self)
    }

    pub fn onenand_image(&self) -> Option<&[u8]> {
        self.onenand_image.as_deref()
    }

    pub(crate) fn from_input(mut input: FirmwareInput) -> anyhow::Result<Self> {
        let eeprom = input.companions.remove("eeprom")
            .ok_or_else(|| anyhow::anyhow!("The E830 NSF EEPROM image is required"))?;
        let nand = input.companions.remove("nand");
        let mut firmware = Self::from_bytes(input.image, eeprom,
            input.companions.remove("tfs_image"), input.companions.remove("tfs_package"))?;
        firmware.onenand_image = nand;
        Ok(firmware)
    }

    pub fn reset_handler(&self) -> u32 {
        self.reset_handler
    }
}

/// The original CLA/NSF format is an adapter, not part of the core CPU or bus.
pub struct ClaLoader;

impl FirmwareLoader for ClaLoader {
    fn id(&self) -> &'static str { "e830-cla" }

    fn selection_hint(&self) -> &'static str {
        "Select the matching .cla firmware and .nsf EEPROM; .tfs is optional."
    }

    fn bundled(&self, root: &Path) -> Option<crate::firmware::FirmwareFiles> {
        super::FirmwarePaths::bundled(root).map(|paths| {
            let mut companions = std::collections::BTreeMap::from([("eeprom".into(), paths.eeprom)]);
            if let Some(tfs) = paths.tfs { companions.insert("tfs_package".into(), tfs); }
            crate::firmware::FirmwareFiles { image: paths.cla, companions }
        })
    }

    fn prepare_files(&self, files: &crate::firmware::FirmwareFiles) -> anyhow::Result<crate::firmware::FirmwareFiles> {
        let mut files = files.clone();
        if !files.companions.contains_key("nand") {
            let sibling = files.image.with_extension("onenand.bin");
            if sibling.is_file() { files.companions.insert("nand".into(), sibling); }
        }
        if let Some(nand) = files.companions.get("nand") {
            crate::devices::e830::validate_nand_image(nand).map_err(anyhow::Error::msg)?;
        }
        Ok(files)
    }

    fn roles(&self) -> Vec<FileRole> {
        vec![
            FileRole { role: "image", extensions: &["cla"], required: true },
            FileRole { role: "eeprom", extensions: &["nsf"], required: true },
            FileRole { role: "tfs_package", extensions: &["tfs"], required: false },
            FileRole { role: "nand", extensions: &["bin"], required: false },
        ]
    }

    fn load(&self, input: FirmwareInput) -> anyhow::Result<LoadedFirmware> {
        use anyhow::{Context, ensure};
        let entry = validate_cla(&input.image)?;
        let eeprom = input.image("eeprom").context("The E830 NSF EEPROM image is required")?;
        let offset = eeprom_payload_offset(eeprom);
        ensure!(offset < eeprom.len(), "EEPROM image is empty");
        for role in ["tfs_image", "tfs_package", "nand"] {
            ensure!(!input.image(role).is_some_and(<[u8]>::is_empty), "{role} image is empty");
        }
        let mut segments = vec![
            ImageSegment { name: "CLA executable NOR".into(), role: "image".into(), offset: 0,
                size: input.image.len(), address: FLASH_BASE.into(), permissions: Permissions::RX },
            ImageSegment { name: "NSF payload".into(), role: "eeprom".into(), offset,
                size: eeprom.len() - offset, address: 0x21c2_0000, permissions: Permissions::RW },
        ];
        if let Some(bytes) = input.image("tfs_image") {
            segments.push(ImageSegment { name: "TFS NOR source".into(), role: "tfs_image".into(),
                offset: 0, size: bytes.len(), address: crate::devices::e830::E830_TFS_SOURCE_BASE.into(),
                permissions: Permissions::RW });
        }
        let metadata = FirmwareMetadata {
            loader: self.id().into(), format: "e830-cla-nsf".into(),
            revision: None, image_sha256: image_sha256(&input.image), image_size: input.image.len(),
            entry, instruction_set: "ARM/Thumb little-endian".into(), segments,
            facts: [("nsf_wrapper".into(), format!("{offset} bytes")),
                ("tfs_package".into(), "Installer source, not a physical NAND image".into())].into(),
            limitations: vec!["DSP and revision-specific HLE retain their existing opt-in gates".into()],
        };
        Ok(LoadedFirmware { metadata, input })
    }
}

fn read_file(path: &Path) -> Result<Vec<u8>, FirmwareError> {
    std::fs::read(path).map_err(|source| FirmwareError::Read {
        path: path.to_path_buf(),
        source,
    })
}

fn eeprom_payload_offset(eeprom: &[u8]) -> usize {
    if eeprom.starts_with(NSF_HEADER_MAGIC) && eeprom.len() >= NSF_HEADER_SIZE {
        NSF_HEADER_SIZE
    } else {
        0
    }
}

fn validate_cla(cla: &[u8]) -> Result<u32, FirmwareError> {
    if cla.len() < RESET_VECTOR_OFFSET + 4 {
        return Err(FirmwareError::ClaTooShort);
    }
    if cla.get(..4) != Some(RESET_VECTOR_INSTRUCTION.as_slice()) {
        return Err(FirmwareError::UnsupportedCla);
    }

    let target = u32::from_le_bytes(
        cla[RESET_VECTOR_OFFSET..RESET_VECTOR_OFFSET + 4]
            .try_into()
            .expect("four-byte reset vector"),
    );
    let start = FLASH_BASE;
    let end = u64::from(FLASH_BASE) + cla.len() as u64;
    if u64::from(target) < u64::from(start) || u64::from(target) >= end {
        return Err(FirmwareError::ResetHandlerOutsideImage { target, start, end });
    }
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::{FirmwareError, FirmwareSet, FLASH_BASE};

    fn test_cla(target: u32) -> Vec<u8> {
        let mut cla = vec![0; 0x200];
        cla[..4].copy_from_slice(&[0x54, 0xf1, 0x9f, 0xe5]);
        cla[0x15c..0x160].copy_from_slice(&target.to_le_bytes());
        cla
    }

    #[test]
    fn e830_image_uses_the_reset_handler_stored_in_its_vector_table() {
        let target = FLASH_BASE + 0x100;
        let images = FirmwareSet::from_bytes(test_cla(target), vec![1, 2, 3], None, None).unwrap();
        assert_eq!(images.reset_handler(), target);
        assert_eq!(images.eeprom(), &[1, 2, 3]);
    }

    #[test]
    fn strips_the_observed_nsf_container_header_from_mapped_eeprom_data() {
        let mut eeprom = vec![0xff; 24];
        eeprom[..3].copy_from_slice(b"NF4");
        eeprom[20..].copy_from_slice(&[1, 2, 3, 4]);
        let images =
            FirmwareSet::from_bytes(test_cla(FLASH_BASE + 0x100), eeprom, None, None).unwrap();
        assert_eq!(images.eeprom_payload(), &[1, 2, 3, 4]);
    }

    #[test]
    fn rejects_an_image_without_the_e830_reset_vector() {
        let error = FirmwareSet::from_bytes(vec![0; 0x200], vec![1], None, None).unwrap_err();
        assert!(matches!(error, FirmwareError::UnsupportedCla));
    }

    #[test]
    fn rejects_a_reset_handler_outside_the_firmware_image() {
        let error =
            FirmwareSet::from_bytes(test_cla(FLASH_BASE + 0x400), vec![1], None, None).unwrap_err();
        assert!(matches!(
            error,
            FirmwareError::ResetHandlerOutsideImage { .. }
        ));
    }

    #[test]
    fn requires_an_eeprom_image() {
        let error =
            FirmwareSet::from_bytes(test_cla(FLASH_BASE + 0x100), vec![], None, None).unwrap_err();
        assert!(matches!(error, FirmwareError::EmptyEeprom));
    }
}
