//! Legacy E830 host paths retained for settings and client compatibility.
use std::path::{Path, PathBuf};
use serde::{Deserialize, Serialize};
use super::{FirmwareError, FirmwareSet};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FirmwarePaths {
    pub cla: PathBuf,
    pub eeprom: PathBuf,
    pub tfs: Option<PathBuf>,
    #[serde(default)]
    pub onenand: Option<PathBuf>,
}

impl FirmwarePaths {
    pub fn bundled(root: &Path) -> Option<Self> {
        let firmware = root.join("firmware");
        let cla = firmware.join("E830XEGG1.cla");
        let eeprom = firmware.join("E830 eeprom.nsf");
        let tfs = firmware.join("E830XEGG1.tfs");
        (cla.is_file() && eeprom.is_file()).then(|| Self {
            cla,
            eeprom,
            tfs: tfs.is_file().then_some(tfs),
            onenand: None,
        })
    }

    pub fn onenand_path(&self) -> Option<PathBuf> {
        self.onenand.clone().or_else(|| {
            let sibling = self.cla.with_extension("onenand.bin");
            sibling.is_file().then_some(sibling)
        })
    }

    pub fn load(&self) -> Result<FirmwareSet, FirmwareError> {
        // The retail TFS file is an installer package, staged separately from NOR.
        let firmware = FirmwareSet::load(&self.cla, &self.eeprom, None, self.tfs.as_deref())?;
        // An explicit GUI selection wins, including when it is missing: report
        // its read error rather than silently booting from a different image.
        if let Some(nand) = self.onenand_path() {
            firmware.with_onenand_image(&nand)
        } else { Ok(firmware) }
    }
}

pub fn classify_firmware_files(paths: Vec<PathBuf>) -> Result<FirmwarePaths, String> {
    let mut cla = None;
    let mut eeprom = None;
    let mut tfs = None;
    for path in paths {
        let extension = path.extension().and_then(|value| value.to_str()).unwrap_or_default().to_ascii_lowercase();
        let destination = match extension.as_str() {
            "cla" => &mut cla,
            "nsf" => &mut eeprom,
            "tfs" => &mut tfs,
            _ => return Err("Select .cla, .nsf and optional .tfs files only.".into()),
        };
        if destination.replace(path).is_some() { return Err("Select only one file of each E830 type.".into()); }
    }
    Ok(FirmwarePaths {
        cla: cla.ok_or_else(|| "The .cla firmware file was not selected.".to_string())?,
        eeprom: eeprom.ok_or_else(|| "The .nsf EEPROM file was not selected.".to_string())?,
        tfs,
        onenand: None,
    })
}
