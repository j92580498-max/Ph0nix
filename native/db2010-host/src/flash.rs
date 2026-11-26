// SPDX-License-Identifier: GPL-2.0-or-later
//! Committed storage for cold boots, published with the VM checkpoint metadata.
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, fs, io::Read, path::{Path, PathBuf}};

const IMAGES: [(&str, u64, u64); 5] = [
    ("main.bin", 0x44020000, 0x45400000 - 0x44020000),
    ("fs.bin", 0x45400000, 0x45f00000 - 0x45400000),
    ("gdfs.bin", 0x45f00000, 0x100000),
    ("nand-main.bin", 0, 65536 * 512),
    ("nand-spare.bin", 0, 65536 * 16),
];

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FlashImages {
    format_version: u32,
    directory: String,
    images: BTreeMap<String, ImageIdentity>,
}

#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ImageIdentity { size_bytes: u64, sha256: String }

fn generation_name(name: &str) -> bool {
    name.strip_prefix("phone-flash-").is_some_and(|suffix|
        !suffix.is_empty() && suffix.bytes().all(|b| b.is_ascii_digit() || b == b'-'))
}

fn identity(path: &Path, size: u64) -> Result<ImageIdentity> {
    let metadata = fs::symlink_metadata(path)?;
    ensure!(metadata.file_type().is_file() && metadata.len() == size,
        "Internal storage image has an invalid size or type: {}", path.display());
    let mut input = fs::File::open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 65536];
    let mut count = 0u64;
    loop {
        let read = input.read(&mut buffer)?;
        if read == 0 { break; }
        count += read as u64;
        ensure!(count <= size, "Internal storage image grew while reading");
        hash.update(&buffer[..read]);
    }
    ensure!(count == size, "Internal storage image changed while reading");
    Ok(ImageIdentity { size_bytes: size, sha256: format!("{:x}", hash.finalize()) })
}

fn inventory(directory: &Path) -> Result<BTreeMap<String, ImageIdentity>> {
    ensure!(fs::symlink_metadata(directory)?.file_type().is_dir(),
        "Internal storage generation is not a directory");
    let names = fs::read_dir(directory)?.map(|entry|
        Ok(entry?.file_name().into_string().map_err(|_| anyhow::anyhow!("Invalid storage filename"))?))
        .collect::<Result<std::collections::BTreeSet<_>>>()?;
    ensure!(names == IMAGES.iter().map(|(name, _, _)| name.to_string()).collect(),
        "Internal storage image inventory differs");
    IMAGES.iter().map(|(name, _, size)|
        Ok((name.to_string(), identity(&directory.join(name), *size)?))).collect()
}

impl FlashImages {
    pub(crate) fn apply(&self, workspace: &Path, state: &mut Value) -> Result<()> {
        ensure!(self.format_version == 1 && generation_name(&self.directory),
            "Invalid internal storage metadata");
        let directory = workspace.join(&self.directory);
        ensure!(inventory(&directory)? == self.images,
            "Internal storage image checksum differs; saved data was kept");
        ensure!(state.get("nand").is_some_and(Value::is_object),
            "Internal storage requires the K750 NAND layout");
        let region = state["regions"].as_array_mut().context("Missing NOR regions")?
            .iter_mut().find(|region| region["address"].as_u64() == Some(0x44000000)
                && region["size"].as_u64() == Some(0x2000000))
            .context("Internal storage requires the K750 NOR layout")?;
        // Preserve any bootstrap prefix below MAIN. The exported MAIN, FS and
        // GDFS replace their original seeds, never overlap them.
        let seeds = region["seeds"].as_array_mut().context("Missing NOR seeds")?;
        seeds.retain(|seed| seed["address"].as_u64().is_some_and(|address| address < 0x44020000));
        for (name, address, _) in &IMAGES[..3] {
            seeds.push(json!({"address": address, "file": directory.join(name)}));
        }
        state["nand"]["nand_main_image"] = json!(directory.join("nand-main.bin"));
        state["nand"]["nand_spare_image"] = json!(directory.join("nand-spare.bin"));
        Ok(())
    }

    pub(crate) fn remove(&self, workspace: &Path) {
        if generation_name(&self.directory) {
            let path = workspace.join(&self.directory);
            if fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_dir()) {
                let _ = fs::remove_dir_all(path);
            }
        }
    }
}

/// An interrupted export cannot replace the previous published generation.
pub(crate) struct FlashExport { path: PathBuf, published: bool }
impl FlashExport {
    pub(crate) fn new(workspace: &Path) -> Result<Self> {
        let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_nanos();
        let path = workspace.join(format!("phone-flash-{nonce}-{}", std::process::id()));
        fs::create_dir(&path)?;
        Ok(Self { path, published: false })
    }
    pub(crate) fn path(&self) -> &Path { &self.path }
    pub(crate) fn seal(&self) -> Result<FlashImages> {
        let images = inventory(&self.path)?;
        for (name, _, _) in IMAGES {
            fs::OpenOptions::new().write(true).open(self.path.join(name))?.sync_all()?;
        }
        crate::qmp::sync_directory(&self.path)?;
        crate::qmp::sync_directory(self.path.parent().context("Storage parent")?)?;
        Ok(FlashImages { format_version: 1,
            directory: self.path.file_name().context("Storage directory")?.to_str()
                .context("Storage directory encoding")?.to_owned(), images })
    }
    pub(crate) fn publish(&mut self) { self.published = true; }
}
impl Drop for FlashExport {
    fn drop(&mut self) { if !self.published { let _ = fs::remove_dir_all(&self.path); } }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Workspace(PathBuf);
    impl Workspace {
        fn new() -> Self {
            let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
            let path = std::env::temp_dir().join(format!("ph0nix-flash-test-{nonce}-{}", std::process::id()));
            fs::create_dir(&path).unwrap(); Self(path)
        }
    }
    impl Drop for Workspace { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); } }
    fn fill(export: &FlashExport) {
        for (name, _, size) in IMAGES {
            let file = fs::File::create(export.path().join(name)).unwrap();
            file.set_len(size).unwrap();
        }
    }
    fn state() -> Value {
        json!({"regions":[{"address":0x44000000u64,"size":0x2000000,
            "seeds":[{"address":0x44000000u64,"file":"prefix"},
                     {"address":0x44020000u64,"file":"stock-main"},
                     {"address":0x45400000u64,"file":"stock-fs"}]}], "nand":{}})
    }
    #[test]
    fn cold_boot_restores_complete_storage_and_rejects_corruption() {
        use std::io::{Seek, SeekFrom, Write};
        let workspace = Workspace::new();
        let mut export = FlashExport::new(&workspace.0).unwrap();
        fill(&export);
        let mut gdfs = fs::OpenOptions::new().write(true).open(export.path().join("gdfs.bin")).unwrap();
        gdfs.seek(SeekFrom::Start(4096)).unwrap(); gdfs.write_all(b"saved settings").unwrap(); drop(gdfs);
        let saved = export.seal().unwrap();
        export.publish(); drop(export);
        let saved: FlashImages = serde_json::from_slice(&serde_json::to_vec(&saved).unwrap()).unwrap();
        let mut boot = state(); saved.apply(&workspace.0, &mut boot).unwrap();
        let seeds = boot["regions"][0]["seeds"].as_array().unwrap();
        assert_eq!(seeds.len(), 4); assert_eq!(seeds[0]["file"], "prefix");
        for (index, (name, address, _)) in IMAGES[..3].iter().enumerate() {
            assert_eq!(seeds[index + 1]["address"], *address);
            assert_eq!(PathBuf::from(seeds[index + 1]["file"].as_str().unwrap()),
                workspace.0.join(&saved.directory).join(name));
        }
        assert!(boot["nand"]["nand_main_image"].as_str().unwrap().ends_with("nand-main.bin"));
        assert!(boot["nand"]["nand_spare_image"].as_str().unwrap().ends_with("nand-spare.bin"));
        let mut invalid_boot = state(); invalid_boot["nand"] = json!([]);
        let original = invalid_boot.clone();
        assert!(saved.apply(&workspace.0, &mut invalid_boot).is_err());
        assert_eq!(invalid_boot, original);
        let path = workspace.0.join(&saved.directory).join("gdfs.bin");
        let mut bytes = fs::read(&path).unwrap(); assert_eq!(&bytes[4096..4110], b"saved settings");
        bytes[4096] ^= 1; fs::write(path, bytes).unwrap();
        let mut boot = state(); let original = boot.clone();
        assert!(saved.apply(&workspace.0, &mut boot).is_err()); assert_eq!(boot, original);
        let mut invalid = saved.clone(); invalid.directory = "../outside".into();
        assert!(invalid.apply(&workspace.0, &mut boot).is_err());
        invalid.remove(&workspace.0); assert!(workspace.0.join(&saved.directory).is_dir());
    }
    #[test]
    fn incomplete_or_failed_export_keeps_previous_generation() {
        let workspace = Workspace::new();
        let mut old = FlashExport::new(&workspace.0).unwrap(); fill(&old);
        old.seal().unwrap(); old.publish(); let old_path = old.path().to_owned(); drop(old);
        let failed_path;
        {
            let failed = FlashExport::new(&workspace.0).unwrap();
            failed_path = failed.path().to_owned();
            fs::write(failed.path().join("gdfs.bin"), b"incomplete").unwrap();
            assert!(failed.seal().is_err());
            fill(&failed); failed.seal().unwrap(); // Simulate a later checkpoint/metadata failure.
        }
        assert!(!failed_path.exists()); assert!(inventory(&old_path).is_ok());
        fs::write(old_path.join("extra.bin"), b"extra").unwrap(); assert!(inventory(&old_path).is_err());
    }
}
