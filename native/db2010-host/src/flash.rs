// SPDX-License-Identifier: GPL-2.0-or-later
//! Committed K750i flash for cold boots; legacy checkpoint images and portable backups.
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, fs, io::{Read, Write}, path::{Path, PathBuf}};

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
        apply_directory(&directory, state)
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

pub(crate) fn apply_directory(directory: &Path, state: &mut Value) -> Result<()> {
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

const MAGIC: &[u8; 8] = b"K750MEM\x01";
const HEADER_BYTES: u64 = MAGIC.len() as u64 + 16 + 32 * IMAGES.len() as u64;
const MEMORY_BYTES: u64 = HEADER_BYTES + IMAGES[0].2 + IMAGES[1].2 + IMAGES[2].2 + IMAGES[3].2 + IMAGES[4].2;
pub(crate) const MEMORY_FILE: &str = "phone-memory.k750mem";

struct MemoryHeader {
    generation: u128,
    hashes: [[u8; 32]; IMAGES.len()],
}

impl MemoryHeader {
    fn write(&self, mut output: impl Write) -> Result<()> {
        output.write_all(MAGIC)?;
        output.write_all(&self.generation.to_le_bytes())?;
        for hash in &self.hashes { output.write_all(hash)?; }
        Ok(())
    }
}

fn open_memory(path: &Path) -> Result<(fs::File, MemoryHeader)> {
    let mut input = fs::File::open(path).context("Opening the K750i memory file")?;
    let metadata = input.metadata()?;
    ensure!(metadata.is_file() && metadata.len() == MEMORY_BYTES, "Invalid K750i memory file size");
    let mut magic = [0; MAGIC.len()];
    input.read_exact(&mut magic)?;
    ensure!(&magic == MAGIC, "Unsupported K750i memory file format");
    let mut generation = [0; 16];
    input.read_exact(&mut generation)?;
    let mut hashes = [[0; 32]; IMAGES.len()];
    for hash in &mut hashes { input.read_exact(hash)?; }
    Ok((input, MemoryHeader { generation: u128::from_le_bytes(generation), hashes }))
}

pub(crate) fn memory_generation(path: &Path) -> Result<u128> {
    Ok(open_memory(path)?.1.generation)
}

fn copy_image(input: &mut impl Read, output: &mut impl Write, mut remaining: u64) -> Result<[u8; 32]> {
    let mut hash = Sha256::new();
    let mut buffer = [0; 65536];
    while remaining != 0 {
        let count = remaining.min(buffer.len() as u64) as usize;
        let chunk = &mut buffer[..count];
        input.read_exact(chunk)?;
        output.write_all(chunk)?;
        hash.update(chunk);
        remaining -= count as u64;
    }
    Ok(hash.finalize().into())
}

fn verify_end(mut input: impl Read) -> Result<()> {
    ensure!(input.read(&mut [0])? == 0, "K750i memory file has unexpected trailing data");
    Ok(())
}

pub(crate) fn unpack_memory(workspace: &Path, path: &Path) -> Result<FlashExport> {
    let (mut input, header) = open_memory(path)?;
    let flash = FlashExport::new(workspace)?;
    for ((name, _, size), expected) in IMAGES.into_iter().zip(header.hashes) {
        let hash = copy_image(&mut input, &mut fs::File::create(flash.path().join(name))?, size)?;
        ensure!(hash == expected, "K750i memory checksum differs for {name}");
    }
    verify_end(input)?;
    Ok(flash)
}

fn pack_memory(output: &mut fs::File, generation: u128,
    mut write_image: impl FnMut(&mut fs::File, &str, u64, u64) -> Result<[u8; 32]>,
) -> Result<()> {
    use std::io::{Seek, SeekFrom};
    let mut header = MemoryHeader { generation, hashes: [[0; 32]; IMAGES.len()] };
    header.write(&mut *output)?;
    for ((name, address, size), hash) in IMAGES.into_iter().zip(&mut header.hashes) {
        *hash = write_image(output, name, address, size)?;
    }
    output.seek(SeekFrom::Start(0))?;
    header.write(output)
}

fn pack_flash_directory(directory: &Path, output: &mut fs::File, generation: u128) -> Result<()> {
    pack_memory(output, generation, |output, name, _, size| {
        let mut input = fs::File::open(directory.join(name))?;
        ensure!(input.metadata()?.len() == size, "Invalid flash image size for {name}");
        let hash = copy_image(&mut input, output, size)?;
        verify_end(input)?;
        Ok(hash)
    })
}

pub(crate) struct PendingMemory { path: PathBuf, generation: u128 }
impl PendingMemory {
    fn create(workspace: &Path) -> Result<(Self, fs::File)> {
        let generation = nonce()?;
        let path = workspace.join(format!("phone-memory-{generation}-{}.pending", std::process::id()));
        let file = fs::OpenOptions::new().write(true).create_new(true).open(&path)?;
        Ok((Self { path, generation }, file))
    }

    pub(crate) fn pack(workspace: &Path, directory: &Path) -> Result<Self> {
        let (pending, mut output) = Self::create(workspace)?;
        pack_flash_directory(directory, &mut output, pending.generation)?;
        output.sync_all()?;
        Ok(pending)
    }

    pub(crate) fn seed(workspace: &Path, mut image: impl FnMut(&str, u64, u64) -> Result<Vec<u8>>) -> Result<Self> {
        let (pending, mut output) = Self::create(workspace)?;
        pack_memory(&mut output, pending.generation, |output, name, address, size| {
            let bytes = image(name, address, size)?;
            ensure!(bytes.len() as u64 == size, "Invalid initial flash image size for {name}");
            copy_image(&mut bytes.as_slice(), output, size)
        })?;
        output.sync_all()?;
        Ok(pending)
    }

    pub(crate) fn import(workspace: &Path, source: &Path) -> Result<Self> {
        let (pending, mut output) = Self::create(workspace)?;
        copy_memory(source, &mut output, Some(pending.generation))?;
        output.sync_all()?;
        Ok(pending)
    }

    pub(crate) fn generation(&self) -> u128 { self.generation }

    pub(crate) fn publish(self, workspace: &Path) -> Result<()> {
        crate::qmp::atomic_replace(&self.path, &workspace.join(MEMORY_FILE))?;
        crate::qmp::sync_directory(workspace)?;
        Ok(())
    }
}
impl Drop for PendingMemory {
    fn drop(&mut self) { let _ = fs::remove_file(&self.path); }
}

fn copy_memory(source: &Path, output: &mut fs::File, generation: Option<u128>) -> Result<()> {
    let (mut input, mut header) = open_memory(source)?;
    if let Some(generation) = generation { header.generation = generation; }
    header.write(&mut *output)?;
    for ((name, _, size), expected) in IMAGES.into_iter().zip(header.hashes) {
        let hash = copy_image(&mut input, output, size)?;
        ensure!(hash == expected, "K750i memory checksum differs for {name}");
    }
    verify_end(input)
}

pub(crate) fn export_memory(source: &Path, destination: &Path) -> Result<()> {
    copy_memory(source, &mut fs::File::create(destination)?, None)
}

impl FlashImages {
    pub(crate) fn export(&self, workspace: &Path, destination: &Path) -> Result<()> {
        ensure!(self.format_version == 1 && generation_name(&self.directory), "Invalid internal storage metadata");
        let directory = workspace.join(&self.directory);
        ensure!(inventory(&directory)? == self.images, "Internal storage image checksum differs");
        pack_flash_directory(&directory, &mut fs::File::create(destination)?, nonce()?)
    }
}

fn nonce() -> Result<u128> {
    Ok(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_nanos())
}

/// An interrupted export cannot replace the previous published generation.
pub(crate) struct FlashExport { path: PathBuf }
impl FlashExport {
    pub(crate) fn new(workspace: &Path) -> Result<Self> {
        let nonce = nonce()?;
        let path = workspace.join(format!("phone-flash-{nonce}-{}", std::process::id()));
        fs::create_dir(&path)?;
        Ok(Self { path })
    }
    pub(crate) fn path(&self) -> &Path { &self.path }
    #[cfg(test)]
    fn seal(&self) -> Result<FlashImages> {
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
}
impl Drop for FlashExport {
    fn drop(&mut self) { let _ = fs::remove_dir_all(&self.path); }
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
        let export = FlashExport::new(&workspace.0).unwrap();
        fill(&export);
        let mut gdfs = fs::OpenOptions::new().write(true).open(export.path().join("gdfs.bin")).unwrap();
        gdfs.seek(SeekFrom::Start(4096)).unwrap(); gdfs.write_all(b"saved settings").unwrap(); drop(gdfs);
        let saved = export.seal().unwrap();
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
        let old = FlashExport::new(&workspace.0).unwrap(); fill(&old);
        old.seal().unwrap(); let old_path = old.path().to_owned();
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
