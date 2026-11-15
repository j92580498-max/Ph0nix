// SPDX-License-Identifier: GPL-2.0-or-later
//! SHA-256 pinned BABE v3 records. Signing trailers and sparse gaps are preserved.
use anyhow::{ensure, Context, Result};
use ::e830::core::memory::Permissions;
use ::e830::firmware::*;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;

pub const FORMAT: &str = "db2010-babe-audited";
pub const MAIN_NAME: &str = "K750_R1DB001_MAIN_EU_1_CL_RED49.mbn";
pub const FS_NAME: &str = "K750_R1DB001_FS_EMEA_1_RED49.fbn";
const CATALOG: &str = include_str!("../../../src/db2010/resources/k750-r1db001-cid49-manifest.json");

#[derive(Deserialize)]
struct Catalog { entries: Vec<Entry> }
#[derive(Deserialize)]
struct Entry {
    id: String, size: usize, sha256: String, base: u64, end: u64,
    record_stream_end: usize, payload_sha256: String,
    segments: Vec<Record>, ranges: Vec<[u64; 2]>,
}
#[derive(Deserialize)]
struct Record { offset: usize, address: u64, length: usize }

pub struct BabeLoader;

fn word(bytes: &[u8], offset: usize) -> Result<u32> {
    Ok(u32::from_le_bytes(bytes.get(offset..offset.checked_add(4).context("record offset overflow")?)
        .context("truncated BABE record")?.try_into()?))
}

fn records(bytes: &[u8], entry: &Entry, role: &str) -> Result<Vec<ImageSegment>> {
    ensure!(bytes.len() == entry.size && image_sha256(bytes) == entry.sha256,
        "{role} does not match the audited K750 R1DB001 CID49 image; this profile cannot run another revision");
    ensure!(bytes.starts_with(&[0xba, 0xbe, 0, 3]) && word(bytes, 0x2e8)? as usize == entry.segments.len(), "BABE v3 record count differs");
    let mut stream = entry.segments.first().context("empty BABE image")?.offset.checked_sub(8).context("invalid record offset")?;
    let mut ranges: Vec<[u64; 2]> = Vec::new();
    let mut payload = Vec::new();
    let mut result = Vec::new();
    for row in &entry.segments {
        let data_start = stream.checked_add(8).context("BABE record overflow")?;
        let data_end = data_start.checked_add(row.length).context("BABE record overflow")?;
        let mapped_end = row.address.checked_add(row.length as u64).context("BABE address overflow")?;
        ensure!(row.length > 0 && row.length <= 65536 && data_start == row.offset && data_end <= bytes.len()
            && word(bytes, stream)? as u64 == row.address && word(bytes, stream + 4)? as usize == row.length
            && entry.base <= row.address && mapped_end <= entry.end, "BABE record mapping differs");
        if let Some(previous) = ranges.last_mut() {
            ensure!(row.address >= previous[1], "BABE records overlap or are unordered");
            if row.address == previous[1] { previous[1] = mapped_end; }
            else { ranges.push([row.address, mapped_end]); }
        } else { ranges.push([row.address, mapped_end]); }
        payload.extend_from_slice(&bytes[data_start..data_end]);
        result.push(ImageSegment {
            name: format!("Original {} record at 0x{:08x}", entry.id, row.address),
            role: role.into(), offset: data_start, size: row.length, address: row.address,
            permissions: Permissions::RX,
        });
        stream = data_end;
    }
    ensure!(stream == entry.record_stream_end && ranges == entry.ranges && image_sha256(&payload) == entry.payload_sha256, "BABE payload coverage differs");
    Ok(result)
}

impl FirmwareLoader for BabeLoader {
    fn id(&self) -> &'static str { "db2010-babe" }
    fn selection_hint(&self) -> &'static str {
        "Select K750 R1DB001 CID49 MAIN .mbn and matching FS .fbn."
    }
    fn roles(&self) -> Vec<FileRole> {
        vec![FileRole { role: "image", extensions: &["mbn"], required: true },
            FileRole { role: "fs", extensions: &["fbn"], required: true }]
    }
    fn bundled(&self, root: &Path) -> Option<FirmwareFiles> {
        let directory = [root.join("fw/k750i/images/R1DB001/cid49/sony-ericsson"), root.join("fw/k750i"),root.join("firmware/k750i")].into_iter()
            .find(|path| path.join(MAIN_NAME).is_file() && path.join(FS_NAME).is_file())?;
        let image = directory.join(MAIN_NAME);
        let fs = directory.join(FS_NAME);
        (image.is_file() && fs.is_file()).then(|| FirmwareFiles { image, companions: BTreeMap::from([("fs".into(), fs)]) })
    }
    fn load(&self, input: FirmwareInput) -> Result<LoadedFirmware> {
        ensure!(input.companions.keys().all(|role| matches!(role.as_str(), "fs" | "gdfs" | "rom")), "Unknown DB2010 companion role");
        let catalog: Catalog = serde_json::from_str(CATALOG)?;
        let main = catalog.entries.iter().find(|entry| entry.id == "main").context("missing MAIN catalog")?;
        let fs = catalog.entries.iter().find(|entry| entry.id == "fs").context("missing FS catalog")?;
        let mut segments = records(&input.image, main, "image")?;
        segments.extend(records(input.image("fs").context("Select the matching K750 FS .fbn")?, fs, "fs")?);
        let vector_offset = main.segments.first().context("missing reset vector")?.offset;
        ensure!(word(&input.image, vector_offset)? == 0xe59f_f018, "Unsupported MAIN reset vector");
        let entry = word(&input.image, vector_offset + 0x20)?;
        ensure!(entry & 3 == 0 && segments.iter().any(|s| s.role == "image" && s.address <= entry as u64 && entry as u64 + 4 <= s.address + s.size as u64), "Reset handler is outside original MAIN code");
        if let Some(gdfs) = input.image("gdfs") {
            ensure!(gdfs.len() == 0x10_0000,
                "GDFS requires a physical 1 MiB NOR bank. The 53,827-byte K750_GDFS.bin is a logical record backup and cannot be memory-mapped");
            segments.push(ImageSegment { name: "Physical GDFS NOR bank".into(), role: "gdfs".into(), offset: 0, size: gdfs.len(), address: 0x45f0_0000, permissions: Permissions::RX });
        }
        if let Some(rom) = input.image("rom") {
            ensure!(rom.len() == 0x1_0000, "DB2010 high ROM dump must contain exactly 64 KiB");
            segments.push(ImageSegment { name: "Supplied high ROM bytes".into(), role: "rom".into(), offset: 0, size: rom.len(), address: 0xffff_0000, permissions: Permissions::RX });
        }
        Ok(LoadedFirmware { metadata: FirmwareMetadata {
            loader: self.id().into(), format: FORMAT.into(), revision: Some("K750 R1DB001 RED49".into()),
            image_sha256: image_sha256(&input.image), image_size: input.image.len(), entry,
            instruction_set: "ARMv5TE ARM/Thumb, little-endian".into(), segments,
            facts: BTreeMap::from([
                ("cpu".into(), "ARM926 profile from danylokos/db2010".into()),
                ("startup".into(), "Direct MAIN reset handler; hardware boot ROM handoff unresolved".into()),
                ("integrity".into(), "Entire containers and mapped payload match the upstream SHA-256 catalog; manufacturer signature verification is not implemented".into()),
                ("sparse_flash".into(), "Only supplied records are established; holes, tails and missing GDFS remain unknown".into()),
                ("key_table".into(), "K750 0x4424734C matches all 31 W800 key translation codes at 0x4423FEA0".into()),
            ]),
            limitations: vec![
                "Native backend is a boot investigation, not a completed handset emulator".into(),
                "The research Unicorn board requires ROM coverage; native apps use the declared compatibility services from db2010-host".into(),
                "DSP, RF, USB, camera, storage controllers and interrupt sources remain incomplete".into(),
                "Initial RAM, register and clock contracts inherit documented DB2010 assumptions; hardware equivalence and undetectability are unverified".into(),
            ],
        }, input })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn malformed_or_wrong_revision_inputs_do_not_create_a_cpu() {
        for image in [vec![], vec![0; 0x300], vec![0xba, 0xbe, 0, 3]] {
            assert!(BabeLoader.load(FirmwareInput { image, companions: BTreeMap::new() }).is_err());
        }
    }
}
