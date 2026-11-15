//! N500 linker-produced flat AXF images and address-linked OGM companions.
//! Neither file is an E830 CLA, and neither has an NF4/NSF wrapper.
use std::collections::BTreeMap;

use anyhow::{ensure, Context, Result};

use super::{
    image_sha256, FileRole, FirmwareInput, FirmwareLoader, FirmwareMetadata, ImageSegment,
    LoadedFirmware,
};
use crate::core::memory::Permissions;
use crate::devices::n500::memory::{EEPROM_SIZE, NOR_BASES, NOR_SIZE};

pub const FORMAT: &str = "n500-flat-axf";
pub const OGM_BASE: u32 = 0x008e_0000;

pub struct AxfLoader;

impl FirmwareLoader for AxfLoader {
    fn id(&self) -> &'static str {
        "n500-axf"
    }

    fn selection_hint(&self) -> &'static str {
        "Select the N500 .axf image and matching .ogm; the .map and 32 KiB .eeprom dump are optional."
    }

    fn bundled(&self, root: &std::path::Path) -> Option<super::FirmwareFiles> {
        let directory = root.join("firmware");
        let image = directory.join("N50XEWK1.axf");
        let mut companions = BTreeMap::new();
        for role in ["ogm", "map", "eeprom"] {
            let path = image.with_extension(role);
            if path.is_file() {
                companions.insert(role.into(), path);
            }
        }
        image
            .is_file()
            .then_some(super::FirmwareFiles { image, companions })
    }

    fn roles(&self) -> Vec<FileRole> {
        vec![
            FileRole {
                role: "image",
                extensions: &["axf"],
                required: true,
            },
            FileRole {
                role: "ogm",
                extensions: &["ogm"],
                required: false,
            },
            FileRole {
                role: "map",
                extensions: &["map"],
                required: false,
            },
            FileRole {
                role: "eeprom",
                extensions: &["eeprom"],
                required: false,
            },
            FileRole {
                role: "flash2",
                extensions: &["nor"],
                required: false,
            },
        ]
    }

    fn load(&self, input: FirmwareInput) -> Result<LoadedFirmware> {
        ensure!(
            !input.image.starts_with(b"\x7fELF"),
            "This N500 loader accepts raw linker AXF images; ELF needs an ELF loader"
        );
        ensure!(
            input.image.len() >= 0x24 && input.image.len() <= NOR_SIZE,
            "The N500 flat AXF must fit its 2 MiB primary NOR and contain reset vectors"
        );
        let reset_target = branch_target(&input.image, 0)?;
        for offset in [4, 8, 12, 16, 24, 28] {
            branch_target(&input.image, offset)?;
        }
        let mut segments = vec![ImageSegment {
            name: "Main NOR, including original reset/startup code".into(),
            role: "image".into(),
            offset: 0,
            size: input.image.len(),
            address: NOR_BASES[0],
            permissions: Permissions::RX,
        }];
        let mut facts = BTreeMap::from([
            ("reset_handler".into(), format!("0x{reset_target:08x}")),
            (
                "cpu_evidence".into(),
                "ARM/Thumb little-endian; VP40575/OneC in service manual".into(),
            ),
            (
                "startup".into(),
                "Original reset code copies flash routine and initializes RAM".into(),
            ),
        ]);
        let mut limitations = vec![
            "Only N50XEWK1 has been exercised; compatible vector layouts do not prove boot support"
                .into(),
            "OneC clocks, reset values and transfer timing are inferred, not verified on hardware"
                .into(),
            "Radio/DSP execution, audio and SIM operation are not implemented".into(),
        ];
        for role in input.companions.keys() {
            ensure!(
                self.roles().iter().any(|item| item.role == role),
                "Unknown companion role {role} for {}",
                self.id()
            );
        }
        if let Some(flash) = input.image("flash2") {
            ensure!(
                flash.len() == NOR_SIZE,
                "A flash2 physical NOR image must be exactly 2 MiB"
            );
            ensure!(
                input.image("ogm").is_none(),
                "Choose an OGM package or a full flash2 dump"
            );
            segments.push(ImageSegment {
                name: "Secondary physical NOR".into(),
                role: "flash2".into(),
                offset: 0,
                size: flash.len(),
                address: NOR_BASES[1],
                permissions: Permissions::RX,
            });
        } else if let Some(ogm) = input.image("ogm") {
            validate_ogm(ogm)?;
            segments.push(ImageSegment {
                name: "OGM address-linked code and resources".into(),
                role: "ogm".into(),
                offset: 0,
                size: ogm.len(),
                address: u64::from(OGM_BASE),
                permissions: Permissions::RX,
            });
            facts.insert(
                "ogm_format".into(),
                "Raw absolute pointer table, Thumb code and data linked at 0x008e0000".into(),
            );
            facts.insert("ogm_sha256".into(), image_sha256(ogm));
            limitations.push(
                "The OGM package does not include user data in the remaining secondary NOR".into(),
            );
        } else {
            limitations
                .push("No OGM or secondary NOR supplied; missing flash cells start erased".into());
        }
        if let Some(eeprom) = input.image("eeprom") {
            ensure!(
                eeprom.len() == EEPROM_SIZE,
                "S524A40X EEPROM dump must be exactly 32 KiB, without an E830 NSF header"
            );
            facts.insert("eeprom".into(), "User-supplied physical 32 KiB dump".into());
        } else {
            facts.insert(
                "eeprom".into(),
                "Fresh erased 32 KiB EEPROM; guest owns initialization".into(),
            );
            limitations.push("No original EEPROM/calibration dump supplied".into());
        }
        if let Some(map) = input.image("map") {
            let map = std::str::from_utf8(map).context("The linker MAP must be text")?;
            ensure!(
                map.starts_with("AREA map of "),
                "Unrecognized ARM linker MAP format"
            );
            facts.insert(
                "linker_map".into(),
                "Companion ARM area map supplied; diagnostic data only".into(),
            );
        }
        let revision = input
            .image
            .split(|byte| *byte == 0)
            .filter_map(|text| std::str::from_utf8(text).ok())
            .find(|text| {
                text.len() == 8
                    && text.starts_with("N50")
                    && text.bytes().all(|b| b.is_ascii_alphanumeric())
            })
            .map(str::to_owned);
        let metadata = FirmwareMetadata {
            loader: self.id().into(),
            format: FORMAT.into(),
            revision,
            image_sha256: image_sha256(&input.image),
            image_size: input.image.len(),
            entry: 0,
            instruction_set: "ARM/Thumb little-endian".into(),
            segments,
            facts,
            limitations,
        };
        let loaded = LoadedFirmware { metadata, input };
        loaded.validate()?;
        Ok(loaded)
    }
}

fn branch_target(image: &[u8], offset: usize) -> Result<u32> {
    let word = u32::from_le_bytes(image[offset..offset + 4].try_into().unwrap());
    ensure!(
        word & 0xff00_0000 == 0xea00_0000,
        "N500 vector at 0x{offset:x} must be an unconditional ARM branch"
    );
    let displacement = ((word << 8) as i32 >> 6) as i64;
    let target = offset as i64 + 8 + displacement;
    ensure!(
        target >= 0x20 && target + 4 <= image.len() as i64 && target & 3 == 0,
        "N500 vector at 0x{offset:x} branches outside the supplied image"
    );
    Ok(target as u32)
}

fn validate_ogm(bytes: &[u8]) -> Result<()> {
    ensure!(
        bytes.len() >= 16 && bytes.len() <= NOR_SIZE - (OGM_BASE as usize & (NOR_SIZE - 1)),
        "OGM package must fit at 0x008e0000 within secondary NOR"
    );
    for pointer in bytes[..16].chunks_exact(4) {
        let pointer = u32::from_le_bytes(pointer.try_into().unwrap()) & !1;
        ensure!(
            pointer >= OGM_BASE && u64::from(pointer) < u64::from(OGM_BASE) + bytes.len() as u64,
            "OGM header contains a pointer outside its 0x008e0000-linked image"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn image() -> FirmwareInput {
        let mut image = vec![0; 0x100];
        for offset in [0, 4, 8, 12, 16, 24, 28] {
            let word = 0xea00_0000 | ((0x40 - offset - 8) / 4) as u32;
            image[offset..offset + 4].copy_from_slice(&word.to_le_bytes());
        }
        FirmwareInput {
            image,
            companions: BTreeMap::new(),
        }
    }
    #[test]
    fn raw_vectors_preserve_entry_and_reject_elf_or_outside_targets() {
        let loaded = AxfLoader.load(image()).unwrap();
        assert_eq!(loaded.metadata.entry, 0);
        assert_eq!(loaded.metadata.segments[0].offset, 0);
        assert_eq!(loaded.metadata.segments[0].address, 0);
        let mut invalid = image();
        invalid.image[..4].copy_from_slice(b"\x7fELF");
        assert!(AxfLoader.load(invalid).is_err());
        let mut invalid = image();
        invalid.image[..4].copy_from_slice(&0xea7f_ffffu32.to_le_bytes());
        assert!(AxfLoader.load(invalid).is_err());
        assert!(AxfLoader
            .load(FirmwareInput {
                image: vec![0; 31],
                companions: BTreeMap::new()
            })
            .is_err());
    }
    #[test]
    fn ogm_link_addresses_and_dump_roles_are_validated() {
        let mut input = image();
        let mut ogm = vec![0; 64];
        for word in ogm[..16].chunks_exact_mut(4) {
            word.copy_from_slice(&(OGM_BASE + 32).to_le_bytes());
        }
        input.companions.insert("ogm".into(), ogm.clone());
        let loaded = AxfLoader.load(input).unwrap();
        assert_eq!(loaded.metadata.segments[1].address, u64::from(OGM_BASE));
        ogm[..4].copy_from_slice(&0x2000_0000u32.to_le_bytes());
        let mut input = image();
        input.companions.insert("ogm".into(), ogm);
        assert!(AxfLoader.load(input).is_err());
        let mut input = image();
        input.companions.insert("eeprom".into(), vec![0; 20]);
        assert!(AxfLoader.load(input).is_err());
        let mut input = image();
        input.companions.insert("nsf".into(), vec![0; 20]);
        assert!(AxfLoader.load(input).is_err());
    }
}
