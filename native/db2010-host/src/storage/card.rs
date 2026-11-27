// SPDX-License-Identifier: GPL-2.0-or-later
//! Raw FAT16 removable-card images and exclusive host-side file operations.
use super::{digest, obex::validate_path, FileEntry, MAX_FILE_BYTES, NONCE};
use anyhow::{ensure, Context, Result};
use fatfs::{FatType, FileSystem, FormatVolumeOptions, FsOptions};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::atomic::Ordering,
};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CardInfo {
    pub path: PathBuf,
    pub size_bytes: u64,
    pub capacity_bytes: u64,
    pub used_bytes: u64,
    pub free_bytes: u64,
    pub filesystem: String,
}
#[derive(Clone, Debug)]
pub struct CardImage {
    path: PathBuf,
}
type Disk<'a> = FileSystem<&'a mut fs::File>;

fn image_path(parts: &[String]) -> Result<String> {
    validate_path(parts)?;
    Ok(parts.join("/"))
}
fn file_path(parts: &[String]) -> Result<String> {
    ensure!(!parts.is_empty(), "Card file name required");
    image_path(parts)
}
fn temp_name(parent: &[String]) -> String {
    let name = format!("PHX{:016X}.TMP", NONCE.fetch_add(1, Ordering::Relaxed));
    if parent.is_empty() {
        name
    } else {
        format!("{}/{name}", parent.join("/"))
    }
}
fn exists(fs: &Disk<'_>, path: &str) -> Result<bool> {
    let (parent, name) = path.rsplit_once('/').unwrap_or(("", path));
    let root = fs.root_dir();
    let directory = if parent.is_empty() {
        root
    } else {
        root.open_dir(parent)?
    };
    for entry in directory.iter() {
        if entry?.file_name().eq_ignore_ascii_case(name) {
            return Ok(true);
        }
    }
    Ok(false)
}

impl CardImage {
    pub fn create(path: &Path, size_mib: u64) -> Result<Self> {
        ensure!(
            (16..=1024).contains(&size_mib),
            "Choose a card size from 16 to 1024 MiB"
        );
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut file = fs::OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(path)
            .context("Creating card image without replacing an existing file")?;
        let result = (|| {
            file.set_len(size_mib * 1024 * 1024)?;
            fatfs::format_volume(
                &mut file,
                FormatVolumeOptions::new()
                    .fat_type(FatType::Fat16)
                    .volume_label(*b"PH0NIX CARD"),
            )?;
            file.sync_all()?;
            Ok(())
        })();
        if let Err(error) = result {
            drop(file);
            let _ = fs::remove_file(path);
            return Err(error);
        }
        drop(file);
        Self::open(path)
    }
    pub fn open(path: &Path) -> Result<Self> {
        let image = Self {
            path: path.to_owned(),
        };
        image.info()?;
        Ok(image)
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    fn with_fs<T>(&self, operation: impl FnOnce(&Disk<'_>, u64) -> Result<T>) -> Result<T> {
        let mut file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&self.path)?;
        ensure!(
            file.metadata()?.is_file(),
            "Choose a regular card image file"
        );
        file.try_lock()
            .map_err(|e| anyhow::anyhow!("Card image is in use: {e}"))?;
        let size = file.metadata()?.len();
        ensure!(
            (16 * 1024 * 1024..=1024 * 1024 * 1024).contains(&size) && size % (32 * 1024) == 0,
            "Card image must be 16 MiB to 1 GiB and aligned to 32 KiB"
        );
        let mut boot = [0; 512];
        file.read_exact(&mut boot)?;
        ensure!(
            boot[510..512] == [0x55, 0xaa] && u16::from_le_bytes([boot[11], boot[12]]) == 512,
            "Card image must contain a raw FAT16 volume (without a VHD or partition header)"
        );
        let sectors16 = u16::from_le_bytes([boot[19], boot[20]]) as u64;
        let sectors = if sectors16 != 0 {
            sectors16
        } else {
            u32::from_le_bytes(boot[32..36].try_into()?) as u64
        };
        ensure!(
            sectors * 512 <= size && sectors > 0,
            "Card filesystem exceeds the image bounds"
        );
        file.seek(SeekFrom::Start(0))?;
        let filesystem =
            FileSystem::new(&mut file, FsOptions::new()).context("Opening card filesystem")?;
        ensure!(
            filesystem.fat_type() == FatType::Fat16,
            "Only FAT16 removable-card images are supported"
        );
        let result = operation(&filesystem, size);
        let unmount = filesystem.unmount();
        let flush = file.sync_all();
        let value = result?;
        unmount?;
        flush?;
        Ok(value)
    }
    pub fn info(&self) -> Result<CardInfo> {
        self.with_fs(|fs, size| {
            let stats = fs.stats()?;
            let cluster_bytes = u64::from(stats.cluster_size());
            let capacity_bytes = u64::from(stats.total_clusters()) * cluster_bytes;
            let free_bytes = u64::from(stats.free_clusters()) * cluster_bytes;
            Ok(CardInfo {
                path: self.path.clone(),
                size_bytes: size,
                capacity_bytes,
                used_bytes: capacity_bytes - free_bytes,
                free_bytes,
                filesystem: "FAT16".into(),
            })
        })
    }
    pub fn list(&self, path: &[String]) -> Result<Vec<FileEntry>> {
        let path = image_path(path)?;
        self.with_fs(|fs, _| {
            let root = fs.root_dir();
            let directory = if path.is_empty() {
                root
            } else {
                root.open_dir(&path)?
            };
            let mut entries = Vec::new();
            for entry in directory.iter() {
                let entry = entry?;
                let name = entry.file_name();
                if name == "." || name == ".." {
                    continue;
                }
                ensure!(entries.len() < 16384, "Card directory has too many entries");
                entries.push(FileEntry {
                    name,
                    directory: entry.is_dir(),
                    size: if entry.is_dir() {
                        None
                    } else {
                        Some(entry.len())
                    },
                    modified: None,
                });
            }
            entries.sort_by(|a, b| {
                b.directory
                    .cmp(&a.directory)
                    .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            });
            Ok(entries)
        })
    }
    pub fn make_directory(&self, path: &[String]) -> Result<()> {
        let path = file_path(path)?;
        self.with_fs(|fs, _| {
            ensure!(!exists(fs, &path)?, "Card folder already exists");
            fs.root_dir().create_dir(&path)?;
            Ok(())
        })
    }
    pub fn delete(&self, path: &[String]) -> Result<()> {
        let path = file_path(path)?;
        self.with_fs(|fs, _| {
            fs.root_dir()
                .remove(&path)
                .context("Removing card file (folders must be empty)")?;
            Ok(())
        })
    }
    pub fn delete_verified(&self, path: &[String], bytes: u64, sha256: &str) -> Result<()> {
        let path = file_path(path)?;
        let expected = super::expected_digest(bytes, sha256)?;
        self.with_fs(|fs, _| {
            let mut source = fs.root_dir().open_file(&path)?;
            ensure!(
                digest(&mut source)? == expected,
                "Source changed since export; the source file was kept"
            );
            drop(source);
            fs.root_dir().remove(&path)?;
            Ok(())
        })
    }
    pub fn import_file(
        &self,
        path: &[String],
        host: &Path,
        replace: bool,
        move_source: bool,
    ) -> Result<()> {
        let target = file_path(path)?;
        let parent = &path[..path.len() - 1];
        let mut source = fs::File::open(host)?;
        ensure!(source.metadata()?.is_file(), "Choose a regular host file");
        let identity = digest(&mut source)?;
        source.seek(SeekFrom::Start(0))?;
        self.with_fs(|fs, _| write_verified(fs, &target, parent, &mut source, identity, replace))?;
        if move_source {
            let mut check = fs::File::open(host)?;
            ensure!(
                digest(&mut check)? == identity,
                "Source changed during transfer; it was kept"
            );
            fs::remove_file(host)?;
        }
        Ok(())
    }
    pub fn export_file(&self, path: &[String], host: &Path, move_source: bool) -> Result<()> {
        let source = file_path(path)?;
        ensure!(!host.exists(), "Host destination already exists");
        if let Some(parent) = host.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut target = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(host)?;
        let mut verified = false;
        let result = self.with_fs(|fs, _| {
            let mut file = fs.root_dir().open_file(&source)?;
            let identity = digest(&mut file)?;
            file.seek(SeekFrom::Start(0))?;
            std::io::copy(&mut file, &mut target)?;
            target.sync_all()?;
            let mut check = fs::File::open(host)?;
            ensure!(
                digest(&mut check)? == identity,
                "Export verification failed; source was kept"
            );
            verified = true;
            if move_source {
                drop(file);
                fs.root_dir().remove(&source)?;
            }
            Ok(())
        });
        if let Err(error) = result {
            drop(target);
            if !verified {
                let _ = fs::remove_file(host);
            }
            return Err(error);
        }
        Ok(())
    }
    pub fn copy(
        &self,
        source: &[String],
        destination: &[String],
        replace: bool,
        move_source: bool,
    ) -> Result<()> {
        let src = file_path(source)?;
        let dst = file_path(destination)?;
        ensure!(
            src.to_lowercase() != dst.to_lowercase(),
            "Choose different source and destination files"
        );
        self.with_fs(|fs, _| {
            let mut file = fs.root_dir().open_file(&src)?;
            let identity = digest(&mut file)?;
            file.seek(SeekFrom::Start(0))?;
            write_verified(
                fs,
                &dst,
                &destination[..destination.len() - 1],
                &mut file,
                identity,
                replace,
            )?;
            drop(file);
            if move_source {
                fs.root_dir().remove(&src)?;
            }
            Ok(())
        })
    }
}

fn write_verified(
    fs: &Disk<'_>,
    destination: &str,
    parent: &[String],
    source: &mut impl Read,
    identity: (u64, [u8; 32]),
    replace: bool,
) -> Result<()> {
    ensure!(
        identity.0 <= MAX_FILE_BYTES as u64,
        "File exceeds 32 MiB transfer limit"
    );
    let present = exists(fs, destination)?;
    ensure!(replace || !present, "Card destination already exists");
    let stats = fs.stats()?;
    ensure!(
        identity.0 <= u64::from(stats.free_clusters()) * u64::from(stats.cluster_size()),
        "Not enough free space on the Memory Stick"
    );
    let staged = temp_name(parent);
    ensure!(!exists(fs, &staged)?, "Staging filename collision");
    let root = fs.root_dir();
    let result = (|| {
        let mut file = root.create_file(&staged)?;
        let count = std::io::copy(&mut source.take(identity.0), &mut file)?;
        file.flush()?;
        ensure!(count == identity.0, "Import source ended early");
        file.seek(SeekFrom::Start(0))?;
        ensure!(
            digest(&mut file)? == identity,
            "Card write verification failed"
        );
        drop(file);
        Ok(())
    })();
    if let Err(error) = result {
        let _ = root.remove(&staged);
        return Err(error);
    }
    if present {
        let backup = temp_name(parent);
        ensure!(!exists(fs, &backup)?, "Backup filename collision");
        root.rename(destination, &root, &backup)?;
        if let Err(error) = root.rename(&staged, &root, destination) {
            let _ = root.rename(&backup, &root, destination);
            let _ = root.remove(&staged);
            return Err(error.into());
        }
        root.remove(&backup)?;
    } else if let Err(error) = root.rename(&staged, &root, destination) {
        let _ = root.remove(&staged);
        return Err(error.into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};
    struct WorkDir(PathBuf);
    impl WorkDir {
        fn new() -> Self {
            let p = std::env::temp_dir().join(format!(
                "ph0nix-card-test-{}-{}",
                std::process::id(),
                NONCE.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&p).unwrap();
            Self(p)
        }
    }
    impl Drop for WorkDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn verified_delete_keeps_same_size_changed_source() {
        let dir = WorkDir::new();
        let card = CardImage::create(&dir.0.join("card.img"), 16).unwrap();
        let source = dir.0.join("source");
        let target = vec!["x.txt".to_owned()];
        fs::write(&source, b"old bytes").unwrap();
        card.import_file(&target, &source, false, false).unwrap();
        let old = Sha256::digest(b"old bytes")
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        fs::write(&source, b"new bytes").unwrap();
        card.import_file(&target, &source, true, false).unwrap();
        assert!(card.delete_verified(&target, 9, &old).is_err());
        assert_eq!(card.list(&[]).unwrap()[0].name, "x.txt");
        let output = dir.0.join("output");
        card.export_file(&target, &output, false).unwrap();
        assert_eq!(fs::read(output).unwrap(), b"new bytes");
        let new = Sha256::digest(b"new bytes")
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        card.delete_verified(&target, 9, &new).unwrap();
        assert!(card.list(&[]).unwrap().is_empty());
        assert!(source.is_file());
    }
    fn path(p: &str) -> Vec<String> {
        p.split('/').map(String::from).collect()
    }
    #[test]
    fn capacity_tracks_fat_allocation_and_excludes_metadata() {
        let dir = WorkDir::new();
        let source = dir.0.join("source");
        fs::write(&source, [42]).unwrap();
        for size_mib in [32, 64] {
            let card = CardImage::create(&dir.0.join(format!("{size_mib}.img")), size_mib).unwrap();
            let empty = card.info().unwrap();
            assert_eq!(empty.size_bytes, size_mib * 1024 * 1024);
            assert!(empty.capacity_bytes < empty.size_bytes);
            assert_eq!(empty.capacity_bytes, empty.free_bytes);
            assert_eq!(empty.used_bytes, 0);
            let cluster = card
                .with_fs(|fs, _| Ok(u64::from(fs.stats()?.cluster_size())))
                .unwrap();
            let target = path("data.bin");
            card.import_file(&target, &source, false, false).unwrap();
            let reopened = CardImage::open(card.path()).unwrap();
            let occupied = reopened.info().unwrap();
            assert_eq!(occupied.capacity_bytes, empty.capacity_bytes);
            assert_eq!(occupied.used_bytes, cluster);
            assert_eq!(occupied.free_bytes + cluster, empty.free_bytes);
            reopened.delete(&target).unwrap();
            assert_eq!(reopened.info().unwrap().free_bytes, empty.free_bytes);
            println!(
                "{size_mib} MiB image: {} FAT data bytes, {cluster} bytes per cluster",
                empty.capacity_bytes
            );
        }
    }
    #[test]
    fn full_card_rejects_an_import_without_changing_contents() {
        let dir = WorkDir::new();
        let card = CardImage::create(&dir.0.join("card.img"), 16).unwrap();
        let source = dir.0.join("source");
        let capacity = card.info().unwrap().capacity_bytes;
        fs::File::create(&source).unwrap().set_len(capacity).unwrap();
        card.import_file(&path("full.bin"), &source, false, false).unwrap();
        assert_eq!(card.info().unwrap().free_bytes, 0);
        fs::write(&source, [42]).unwrap();
        let error = card.import_file(&path("overflow.bin"), &source, false, false).unwrap_err();
        assert!(error.to_string().contains("Not enough free space"));
        let entries = card.list(&[]).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "full.bin");
        assert_eq!(entries[0].size, Some(capacity));
        assert_eq!(card.info().unwrap().free_bytes, 0);
    }
    #[test]
    fn card_survives_reopen_and_verified_moves() {
        let dir = WorkDir::new();
        let card = CardImage::create(&dir.0.join("card.img"), 16).unwrap();
        assert_eq!(card.info().unwrap().filesystem, "FAT16");
        card.make_directory(&path("Music")).unwrap();
        let source = dir.0.join("source.bin");
        let bytes: Vec<u8> = (0..100000).map(|n| (n * 37) as u8).collect();
        fs::write(&source, &bytes).unwrap();
        card.import_file(&path("Music/échantillon.bin"), &source, false, false)
            .unwrap();
        assert!(card
            .import_file(&path("Music/échantillon.bin"), &source, false, false)
            .is_err());
        let card = CardImage::open(card.path()).unwrap();
        assert_eq!(
            card.list(&path("Music")).unwrap()[0].name,
            "échantillon.bin"
        );
        card.copy(
            &path("Music/échantillon.bin"),
            &path("copied.bin"),
            false,
            true,
        )
        .unwrap();
        assert!(card.list(&path("Music")).unwrap().is_empty());
        let output = dir.0.join("export.bin");
        card.export_file(&path("copied.bin"), &output, true)
            .unwrap();
        assert_eq!(fs::read(output).unwrap(), bytes);
        assert!(card
            .list(&[])
            .unwrap()
            .iter()
            .all(|e| e.name != "copied.bin"));
        card.import_file(&path("new.bin"), &source, false, true)
            .unwrap();
        assert!(!source.exists());
    }
    #[test]
    fn corrupt_images_and_existing_outputs_are_refused() {
        let dir = WorkDir::new();
        let image = dir.0.join("card.img");
        let card = CardImage::create(&image, 16).unwrap();
        assert!(CardImage::create(&image, 16).is_err());
        assert!(CardImage::create(&dir.0.join("small.img"), 1).is_err());
        assert!(card.make_directory(&path("../escape")).is_err());
        let mut file = fs::OpenOptions::new().write(true).open(&image).unwrap();
        file.seek(SeekFrom::Start(510)).unwrap();
        file.write_all(&[0, 0]).unwrap();
        assert!(CardImage::open(&image).is_err());
    }
    #[test]
    fn replacement_is_verified_and_keeps_original_on_short_source() {
        let dir = WorkDir::new();
        let card = CardImage::create(&dir.0.join("card.img"), 16).unwrap();
        let source = dir.0.join("source");
        fs::write(&source, b"old").unwrap();
        card.import_file(&path("x.txt"), &source, false, false)
            .unwrap();
        let failed = card.with_fs(|fs, _| {
            write_verified(fs, "x.txt", &[], &mut &b"short"[..], (100, [0; 32]), true)
        });
        assert!(failed.is_err());
        let output = dir.0.join("out");
        card.export_file(&path("x.txt"), &output, false).unwrap();
        assert_eq!(fs::read(output).unwrap(), b"old");
        fs::write(&source, b"new").unwrap();
        card.import_file(&path("x.txt"), &source, true, false)
            .unwrap();
        assert_eq!(card.list(&[]).unwrap().len(), 1);
    }
}
