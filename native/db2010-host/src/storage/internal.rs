// SPDX-License-Identifier: GPL-2.0-or-later
//! Offline access to the user directory in a committed K750i memory file.
use super::{obex::validate_path, FileEntry, MAX_FILE_BYTES};
use crate::{flash::{self, PendingMemory, MEMORY_FILE}, qmp::remove_old_checkpoint};
use anyhow::{ensure, Context, Result};
use fatfs::{FatType, FileSystem, FsOptions};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    fmt, fs,
    io,
    path::{Path, PathBuf},
};

#[derive(Debug)]
pub struct InUse;

impl fmt::Display for InUse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Turn off K750i before accessing its internal memory.")
    }
}
impl std::error::Error for InUse {}

#[derive(Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    Info { path: Vec<String> },
    List { path: Vec<String> },
    Export { path: Vec<String>, host: PathBuf },
    Import { path: Vec<String>, host: PathBuf, replace: bool, generation: Option<String> },
    MakeDirectory { path: Vec<String> },
    Delete { path: Vec<String> },
}

impl Request {
    fn path(&self) -> &[String] {
        match self {
            Self::Info { path } | Self::List { path } | Self::Export { path, .. }
            | Self::Import { path, .. } | Self::MakeDirectory { path } | Self::Delete { path } => path,
        }
    }
}

pub fn request(workspace: &Path, request: Request) -> Result<Value> {
    ensure!(workspace.is_dir(), "No saved K750i internal memory. Run the phone and return to Library first.");
    let lease = fs::OpenOptions::new().create(true).truncate(false).read(true).write(true)
        .open(workspace.join("session.lock"))?;
    match lease.try_lock() {
        Ok(()) => (),
        Err(fs::TryLockError::WouldBlock) => return Err(InUse.into()),
        Err(fs::TryLockError::Error(error)) => return Err(error).context("Locking K750i internal memory"),
    }
    let memory = workspace.join(MEMORY_FILE);
    let generation = format!("{:032x}", flash::memory_generation(&memory)?);
    let parts = request.path();
    validate_path(parts)?;
    if matches!(request, Request::Info { .. }) {
        return Ok(json!({"generation":generation}));
    }
    ensure!(matches!(request, Request::List { .. }) || !parts.is_empty(), "Select a file or folder within internal memory");
    if let Request::Import { generation: Some(expected), .. } = &request {
        ensure!(*expected == generation, "K750i memory changed while this file was open. Reopen the file.");
    }
    let path = parts.join("/");
    let flash = flash::unpack_memory(workspace, &memory)?;
    let mut volume = Volume::open(flash.path())?;
    let disk = FileSystem::new(volume.disk(), FsOptions::new())
        .context("Opening K750i internal FAT volume")?;
    ensure!(disk.fat_type() == FatType::Fat16, "Unsupported K750i internal FAT volume");
    let mut entries = Vec::new();
    let mut exported = None;
    let writes;
    {
        let user = disk.root_dir().open_dir("tpa/user").context("Opening K750i user directory")?;
        writes = match request {
            Request::List { .. } => {
                let directory = if path.is_empty() { user } else { user.open_dir(&path)? };
                for entry in directory.iter() {
                    let entry = entry?;
                    let name = entry.file_name();
                    if name == "." || name == ".." || validate_path(std::slice::from_ref(&name)).is_err() { continue; }
                    ensure!(entries.len() < 16384, "K750i internal directory has too many entries");
                    entries.push(FileEntry {
                        name, directory: entry.is_dir(), size: entry.is_file().then_some(entry.len()), modified: None,
                    });
                }
                entries.sort_by(|a, b| b.directory.cmp(&a.directory)
                    .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
                false
            }
            Request::Export { host, .. } => {
                let mut input = user.open_file(&path)?;
                let mut output = fs::OpenOptions::new().write(true).create_new(true).open(&host)?;
                if let Err(error) = io::copy(&mut input, &mut output) {
                    drop(output);
                    let _ = fs::remove_file(host);
                    return Err(error).context("Reading K750i file");
                }
                exported = Some(host);
                false
            }
            Request::Import { host, replace, generation, .. } => {
                use io::{Read, Write};
                let mut source = fs::File::open(host)?;
                let metadata = source.metadata()?;
                ensure!(metadata.is_file() && metadata.len() <= MAX_FILE_BYTES as u64, "Choose a regular file up to 32 MiB");
                let existing = user.open_file(&path);
                let mut file = if replace {
                    ensure!(generation.is_some(), "An open K750i file must have a memory generation");
                    let mut file = existing.context("Reopening K750i file for writing")?;
                    file.truncate()?;
                    file
                } else {
                    match existing {
                        Err(ref e) if e.kind() == io::ErrorKind::NotFound => (),
                        Err(e) => return Err(e.into()),
                        Ok(_) => anyhow::bail!("A file with that name already exists"),
                    }
                    user.create_file(&path)?
                };
                let copied = io::copy(&mut Read::by_ref(&mut source).take(metadata.len() + 1), &mut file)?;
                ensure!(copied == metadata.len(), "The source file changed while copying");
                file.flush()?;
                true
            }
            Request::MakeDirectory { .. } => {
                match user.open_dir(&path) {
                    Err(ref e) if e.kind() == io::ErrorKind::NotFound => (),
                    Err(e) => return Err(e.into()),
                    Ok(_) => anyhow::bail!("A folder with that name already exists"),
                }
                user.create_dir(&path)?;
                true
            }
            Request::Delete { .. } => { user.remove(&path)?; true }
            Request::Info { .. } => unreachable!(),
        };
    }
    if let Err(error) = disk.unmount() {
        if let Some(host) = exported { let _ = fs::remove_file(host); }
        return Err(error).context("Closing K750i internal FAT volume");
    }
    if writes {
        volume.save(flash.path())?;
        let pending = PendingMemory::pack(workspace, flash.path(), None)?;
        let generation = format!("{:032x}", pending.generation());
        pending.publish(workspace)?;
        remove_old_checkpoint(workspace);
        return Ok(json!({"generation":generation}));
    }
    Ok(json!({"generation":generation, "snapshot":{"entries":entries}}))
}

mod volume;
use volume::Volume;
