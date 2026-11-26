// SPDX-License-Identifier: GPL-2.0-or-later
//! MAIN/FS authentication and sparse records are independent of the app UI.
use anyhow::{ensure, Context, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{fs, io::{Cursor, Read}, path::{Path, PathBuf}};
use zip::ZipArchive;

const BUNDLE: &[u8] = include_bytes!("../resources/k750-compatibility.zip");

#[derive(Deserialize)]
struct Manifest { catalog: Catalog, fs: FsLayout, main_patches: Vec<Patch>, compatibility_files: std::collections::BTreeMap<String, Identity> }
#[derive(Deserialize)]
struct Catalog { entries: Vec<Entry> }
#[derive(Deserialize)]
struct Identity { size: usize, sha256: String }
#[derive(Deserialize)]
struct Entry {
    id:String, size:usize, sha256:String, base:u64, end:u64,
    record_stream_end:usize, payload_sha256:String, segments:Vec<Record>, ranges:Vec<[u64;2]>,
}
#[derive(Deserialize)]
struct Record { offset:usize, address:u64, length:usize }
#[derive(Deserialize)]
struct FsLayout { base:u64, size:usize, stock_sha256:String, provisioned_sha256:String, patches:Vec<Patch> }
#[derive(Deserialize)]
struct Patch { address:u64, original_hex:String, value_hex:String }

fn unhex(text: &str) -> Result<Vec<u8>> {
    ensure!(text.len() % 2 == 0, "odd hex data");
    text.as_bytes().chunks_exact(2).map(|pair| {
        let high = (pair[0] as char).to_digit(16).context("invalid hex data")?;
        let low = (pair[1] as char).to_digit(16).context("invalid hex data")?;
        Ok(((high << 4) | low) as u8)
    }).collect()
}

fn sha(bytes: &[u8]) -> String { format!("{:x}", Sha256::digest(bytes)) }
fn word(bytes: &[u8], offset:usize) -> Result<u32> {
    Ok(u32::from_le_bytes(bytes.get(offset..offset.checked_add(4).context("offset overflow")?)
        .context("truncated BABE record")?.try_into()?))
}
fn records(bytes:&[u8], entry:&Entry) -> Result<Vec<(u64,Vec<u8>)>> {
    ensure!(bytes.len()==entry.size && sha(bytes)==entry.sha256,
        "{} is not the supported K750 R1DB001 CID49 image", entry.id.to_uppercase());
    ensure!(bytes.starts_with(&[0xba,0xbe,0,3]) && word(bytes,0x2e8)? as usize==entry.segments.len(), "BABE header differs");
    let mut stream=entry.segments.first().context("empty BABE image")?.offset.checked_sub(8).context("record offset")?;
    let mut result:Vec<(u64,Vec<u8>)>=Vec::new();
    let mut payload=Sha256::new();
    for row in &entry.segments {
        let start=stream.checked_add(8).context("record overflow")?;
        let end=start.checked_add(row.length).context("record overflow")?;
        let mapped=row.address.checked_add(row.length as u64).context("address overflow")?;
        ensure!(row.length>0 && row.length<=65536 && start==row.offset && end<=bytes.len() &&
            word(bytes,stream)? as u64==row.address && word(bytes,stream+4)? as usize==row.length &&
            entry.base<=row.address && mapped<=entry.end, "BABE record coverage differs");
        payload.update(&bytes[start..end]);
        if let Some((address,data))=result.last_mut() {
            let previous=*address+data.len() as u64;
            ensure!(row.address>=previous,"overlapping BABE records");
            if row.address==previous { data.extend_from_slice(&bytes[start..end]); }
            else { result.push((row.address,bytes[start..end].to_vec())); }
        } else { result.push((row.address,bytes[start..end].to_vec())); }
        stream=end;
    }
    let ranges:Vec<_>=result.iter().map(|(address,data)|[*address,*address+data.len() as u64]).collect();
    ensure!(stream==entry.record_stream_end && ranges==entry.ranges &&
        format!("{:x}",payload.finalize())==entry.payload_sha256,"BABE payload identity differs");
    Ok(result)
}
fn member(archive:&mut ZipArchive<Cursor<&[u8]>>, name:&str) -> Result<Vec<u8>> {
    let mut file=archive.by_name(name)?;
    ensure!(file.size()<=40*1024*1024,"native compatibility member is too large");
    let mut data=Vec::with_capacity(file.size() as usize);
    file.read_to_end(&mut data)?; Ok(data)
}

fn patch_records(records: &mut [(u64, Vec<u8>)], patches: &[Patch]) -> Result<()> {
    for patch in patches {
        let original = unhex(&patch.original_hex)?;
        let value = unhex(&patch.value_hex)?;
        ensure!(original.len() == value.len() && !value.is_empty(), "MAIN compatibility patch length differs");
        let end = patch.address.checked_add(value.len() as u64).context("MAIN compatibility patch overflow")?;
        let (base, bytes) = records.iter_mut().find(|(base, bytes)|
            *base <= patch.address && end <= *base + bytes.len() as u64)
            .context("MAIN compatibility patch lacks authenticated record coverage")?;
        let offset = (patch.address - *base) as usize;
        let target = &mut bytes[offset..offset + value.len()];
        ensure!(target == original, "MAIN compatibility source bytes differ");
        target.copy_from_slice(&value);
    }
    Ok(())
}

pub fn validate(main:&Path, fs_image:&Path) -> Result<()> {
    let mut archive=ZipArchive::new(Cursor::new(BUNDLE))?;
    let manifest:Manifest=serde_json::from_slice(&member(&mut archive,"manifest.json")?)?;
    for (role,path) in [("main",main),("fs",fs_image)] {
        let entry=manifest.catalog.entries.iter().find(|e|e.id==role).context("missing image catalog")?;
        records(&fs::read(path).with_context(||format!("Reading {}",path.display()))?,entry)?;
    }
    Ok(())
}

pub struct Prepared {
    pub state: PathBuf,
    pub lease: fs::File,
}

pub fn prepare(main:&Path, fs_image:&Path, data_dir:&Path) -> Result<PathBuf> {
    Ok(prepare_locked(main,fs_image,data_dir)?.state)
}

/// Locate the same private storage as a normal boot without provisioning firmware.
pub fn workspace(main:&Path, fs_image:&Path, data_dir:&Path) -> Result<PathBuf> {
    let mut archive=ZipArchive::new(Cursor::new(BUNDLE))?;
    let manifest:Manifest=serde_json::from_slice(&member(&mut archive,"manifest.json")?)?;
    let (main_entry,fs_entry)=catalog_pair(&manifest)?;
    workspace_for(main,fs_image,data_dir,main_entry,fs_entry)
}

fn catalog_pair(manifest:&Manifest) -> Result<(&Entry,&Entry)> {
    let main=manifest.catalog.entries.iter().find(|e|e.id=="main").context("MAIN catalog")?;
    let fs=manifest.catalog.entries.iter().find(|e|e.id=="fs").context("FS catalog")?;
    Ok((main,fs))
}

fn workspace_for(main:&Path, fs_image:&Path, data_dir:&Path, main_entry:&Entry, fs_entry:&Entry) -> Result<PathBuf> {
    let identity=sha(format!("{}\n{}\n{}\n{}\n{}",sha(BUNDLE),main_entry.sha256,fs_entry.sha256,
        fs::canonicalize(main)?.display(),fs::canonicalize(fs_image)?.display()).as_bytes());
    Ok(data_dir.join("k750i").join(&identity[..24]))
}

fn provision(main:&Path, fs_image:&Path, manifest:&Manifest) -> Result<(Vec<(u64,Vec<u8>)>, Vec<u8>)> {
    let main_bytes=fs::read(main).with_context(||format!("Reading {}",main.display()))?;
    let fs_bytes=fs::read(fs_image).with_context(||format!("Reading {}",fs_image.display()))?;
    let (main_entry,fs_entry)=catalog_pair(manifest)?;
    let mut main_records=records(&main_bytes,main_entry)?;
    // Apply only declared, byte-checked compatibility to the private mapped
    // copy after authenticating every original container and payload byte.
    patch_records(&mut main_records, &manifest.main_patches)?;
    let fs_records=records(&fs_bytes,fs_entry)?;
    let mut nor_fs=vec![255;manifest.fs.size];
    for (address,bytes) in fs_records {
        let offset=address.checked_sub(manifest.fs.base).context("FS address")? as usize;
        nor_fs.get_mut(offset..offset+bytes.len()).context("FS region bounds")?.copy_from_slice(&bytes);
    }
    ensure!(sha(&nor_fs)==manifest.fs.stock_sha256,"stock FS assembly differs");
    for patch in &manifest.fs.patches {
        let original = unhex(&patch.original_hex)?;
        let value = unhex(&patch.value_hex)?;
        ensure!(original.len()==value.len() && !value.is_empty(),"FS provisioning patch length differs");
        let offset = patch.address.checked_sub(manifest.fs.base).context("FS provisioning address")? as usize;
        let end = offset.checked_add(value.len()).context("FS provisioning overflow")?;
        let target = nor_fs.get_mut(offset..end).context("FS provisioning bounds")?;
        ensure!(target==original,"FS provisioning source bytes differ");
        target.copy_from_slice(&value);
    }
    ensure!(sha(&nor_fs)==manifest.fs.provisioned_sha256,"provisioned FS identity differs");
    Ok((main_records, nor_fs))
}

fn compatibility(archive:&mut ZipArchive<Cursor<&[u8]>>, name:&str, identity:&Identity) -> Result<Vec<u8>> {
    let bytes=member(archive,name)?;
    ensure!(bytes.len()==identity.size && sha(&bytes)==identity.sha256,"native compatibility input {name} differs");
    Ok(bytes)
}

/// Create a portable memory file for a new phone, using the same authenticated
/// flash bytes as its first boot. The caller supplies a fresh private directory.
pub fn create_internal_memory(main:&Path, fs_image:&Path, workspace:&Path) -> Result<()> {
    let mut archive=ZipArchive::new(Cursor::new(BUNDLE))?;
    let manifest:Manifest=serde_json::from_slice(&member(&mut archive,"manifest.json")?)?;
    let (main_records, mut nor_fs)=provision(main,fs_image,&manifest)?;
    fs::create_dir(workspace)?;
    crate::flash::PendingMemory::seed(workspace, |name, address, size| match name {
        "main.bin" => {
            let mut bytes=vec![255;size as usize];
            for (base, record) in &main_records {
                let offset=base.checked_sub(address).context("MAIN region address")? as usize;
                bytes.get_mut(offset..offset+record.len()).context("MAIN region bounds")?.copy_from_slice(record);
            }
            Ok(bytes)
        }
        "fs.bin" => Ok(std::mem::take(&mut nor_fs)),
        _ => compatibility(&mut archive,name,manifest.compatibility_files.get(name).context("Initial flash image")?),
    })?.publish(workspace)
}

pub fn prepare_locked(main:&Path, fs_image:&Path, data_dir:&Path) -> Result<Prepared> {
    prepare_in(main,fs_image,|manifest| {
        let (main_entry,fs_entry)=catalog_pair(manifest)?;
        workspace_for(main,fs_image,data_dir,main_entry,fs_entry)
    })
}

pub fn prepare_locked_at(main:&Path, fs_image:&Path, workspace:&Path) -> Result<Prepared> {
    prepare_in(main,fs_image,|_| Ok(workspace.to_owned()))
}

fn prepare_in(main:&Path, fs_image:&Path, locate:impl FnOnce(&Manifest) -> Result<PathBuf>) -> Result<Prepared> {
    let mut archive=ZipArchive::new(Cursor::new(BUNDLE))?;
    let manifest:Manifest=serde_json::from_slice(&member(&mut archive,"manifest.json")?)?;
    let (main_records,nor_fs)=provision(main,fs_image,&manifest)?;
    let workspace=locate(&manifest)?;
    fs::create_dir_all(&workspace)?;
    let lease=fs::OpenOptions::new().create(true).truncate(false).read(true).write(true)
        .open(workspace.join("session.lock"))?;
    lease.lock().context("Waiting for the previous native phone session")?;
    let mut state:Value=serde_json::from_slice(&member(&mut archive,"state-template.json")?)?;
    let mut seeds=Vec::new();
    for (index,(address,bytes)) in main_records.into_iter().enumerate() {
        let file=workspace.join(format!("main-{index}.bin"));
        fs::write(&file,bytes)?;
        seeds.push(json!({"address":address,"file":file}));
    }
    let fs_file=workspace.join("fs.bin"); fs::write(&fs_file,nor_fs)?;
    seeds.push(json!({"address":manifest.fs.base,"file":fs_file}));
    for (name,identity) in &manifest.compatibility_files {
        fs::write(workspace.join(name),compatibility(&mut archive,name,identity)?)?;
    }
    seeds.push(json!({"address":0x45f00000u64,"file":workspace.join("gdfs.bin")}));
    state["regions"][0]["seeds"]=json!(seeds);
    for (key,name) in [("nand_main_image","nand-main.bin"),("nand_spare_image","nand-spare.bin"),
                       ("prepared_spare","nand-known-spare.bin")] {
        state["nand"][key]=json!(workspace.join(name));
    }
    let path=workspace.join("state.json");
    if workspace.join("phone.vmstate").is_file() {
        if let Ok(old)=fs::read(&path).and_then(|bytes|serde_json::from_slice::<Value>(&bytes)
            .map_err(std::io::Error::other)) {
            if let Some(layout)=old.get("checkpoint_tcm") {state["checkpoint_tcm"]=layout.clone();}
        }
    }
    fs::write(&path,serde_json::to_vec_pretty(&state)?)?;
    Ok(Prepared {state:path,lease})
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn compatibility_bundle_contains_no_main_or_fs_and_keeps_all_identities() {
        let mut archive=ZipArchive::new(Cursor::new(BUNDLE)).unwrap();
        let manifest:Manifest=serde_json::from_slice(&member(&mut archive,"manifest.json").unwrap()).unwrap();
        assert!(archive.by_name("main.bin").is_err()); assert!(archive.by_name("fs.bin").is_err());
        for (name,identity) in manifest.compatibility_files {
            let bytes=member(&mut archive,&name).unwrap();
            assert_eq!(bytes.len(),identity.size); assert_eq!(sha(&bytes),identity.sha256);
        }
        assert!(manifest.fs.patches.iter().all(|p|p.address>=manifest.fs.base && p.address<manifest.fs.base+manifest.fs.size as u64));
        assert!(manifest.fs.patches.iter().any(|p|p.address<0x45e00404)); // CDA installation cannot be omitted.
        assert!(manifest.fs.patches.iter().all(|p|p.original_hex.len()==p.value_hex.len()));
        assert_ne!(manifest.fs.stock_sha256,manifest.fs.provisioned_sha256);
        assert_eq!(manifest.main_patches.len(), 1);
        assert_eq!(manifest.main_patches[0].address, 0x44027130);
    }

    #[test]
    fn java_compatibility_requires_exact_authenticated_source_and_preserves_other_bytes() {
        let patch = Patch { address: 0x44027130, original_hex: "c04991e8".into(), value_hex: "2c119fe5".into() };
        let mut mapped = vec![(0x4402712c, vec![1,2,3,4,0xc0,0x49,0x91,0xe8,5,6,7,8])];
        patch_records(&mut mapped, std::slice::from_ref(&patch)).unwrap();
        assert_eq!(mapped[0].1, [1,2,3,4,0x2c,0x11,0x9f,0xe5,5,6,7,8]);
        assert!(patch_records(&mut mapped, std::slice::from_ref(&patch)).is_err());
        assert!(patch_records(&mut [(0x44027134, vec![0;4])], &[patch]).is_err());
    }
}
