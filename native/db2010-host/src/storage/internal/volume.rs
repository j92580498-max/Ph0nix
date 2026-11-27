// SPDX-License-Identifier: GPL-2.0-or-later
//! R1DB001's first FAT16 volume spans committed NOR slots and NAND page pairs.
use anyhow::{ensure, Context, Result};
use std::{fs, path::Path};

const NOR_SPAN: usize = 0x40000;
const NOR_SLOT: usize = 1028;
const SECTOR: usize = 512;
const PAIR: usize = 2 * SECTOR;
const SPARE: usize = 16;
const PAGES: usize = 65536;
const BASE: usize = 11;
const TOTAL: usize = 81262;
const FAT_SECTORS: usize = 159;
const METADATA_PAIRS: usize = 96;
const FIRST_DATA_PAIR: usize = METADATA_PAIRS;

#[derive(Clone, Copy)]
enum Slot {
    Nor(usize),
    Nand(usize),
}

pub(super) struct Volume {
    nor: Vec<u8>,
    main: Vec<u8>,
    spare: Vec<u8>,
    slots: Vec<Option<Slot>>,
    free: Vec<usize>,
    bytes: Vec<u8>,
}

impl Volume {
    pub(super) fn open(path: &Path) -> Result<Self> {
        let nor = fs::read(path.join("fs.bin"))?;
        let main = fs::read(path.join("nand-main.bin"))?;
        let spare = fs::read(path.join("nand-spare.bin"))?;
        ensure!(nor.len() == 44 * NOR_SPAN && main.len() == PAGES * SECTOR && spare.len() == PAGES * SPARE,
            "Unsupported K750i flash image size");
        let mut slots = vec![None; TOTAL / 2];
        let mut seeded = false;
        'nor: for (span_no, span) in nor.chunks_exact(NOR_SPAN).enumerate() {
            ensure!(span[NOR_SPAN - 2..] == [0x54, 0x12], "Invalid K750i NOR footer");
            for (i, slot) in span[..255 * NOR_SLOT].chunks_exact(NOR_SLOT).enumerate() {
                let tag = u32::from_le_bytes(slot[PAIR..].try_into().unwrap());
                if tag == 0xabcd1254 {
                    seeded = true;
                    break 'nor;
                }
                match tag >> 24 {
                    0xaa => record(&mut slots, tag, Slot::Nor(span_no * NOR_SPAN + i * NOR_SLOT))?,
                    0 | 0xbf | 0xff => (),
                    _ => anyhow::bail!("Unsupported K750i NOR slot tag"),
                }
            }
        }
        let mut free = Vec::new();
        let mut live_nand = false;
        for (i, pair) in spare.chunks_exact(2 * SPARE).enumerate() {
            let page = i * 2;
            let tag = u32::from_le_bytes(pair[..4].try_into().unwrap());
            match tag >> 24 {
                0xaa => {
                    ensure!(!seeded, "K750i flash contains both factory and live slot markers");
                    live_nand = true;
                    free.clear();
                    if record(&mut slots, tag, Slot::Nand(page))? {
                        validate_nand_pair(&main, pair, page)?;
                    }
                }
                0 => free.clear(), // Stop using earlier holes in this append area.
                0xbf if tag == 0xbfffffff && erased(&pair[4..14]) && erased(&pair[16..])
                    && (pair[14..16] == [0xff, 0xff] || (page % 32 == 0 && pair[14..16] == [0x54, 0x12])) => {
                    let data = &main[page * SECTOR..(page + 2) * SECTOR];
                    if erased(data) { free.push(page); }
                }
                0xbf | 0xff => (),
                _ => anyhow::bail!("Unsupported K750i NAND slot tag"),
            }
        }
        ensure!(!seeded && live_nand, "Start K750i once, finish setup, and return to the library before opening Internal memory.");
        let mut bytes = vec![0; TOTAL * SECTOR];
        let mut volume = Self { nor, main, spare, slots, free, bytes: Vec::new() };
        for (i, destination) in bytes.chunks_exact_mut(PAIR).enumerate() {
            if let Some(source) = volume.get(i) { destination.copy_from_slice(source); }
        }
        ensure!(bytes[3..11] == *b"MSWIN4.1"
            && bytes[11..19] == [0, 2, 2, 1, 0, 1, 0, 2]
            && bytes[19..24] == [0, 0, 0xf8, 0x9f, 0]
            && bytes[32..36] == (TOTAL as u32).to_le_bytes()
            && bytes[510..512] == [0x55, 0xaa], "Unsupported K750i internal FAT geometry");
        ensure!(volume.slots[..METADATA_PAIRS].iter().all(Option::is_some), "Missing K750i filesystem metadata");
        let fat = &bytes[SECTOR..(1 + FAT_SECTORS) * SECTOR];
        for (i, slot) in volume.slots[FIRST_DATA_PAIR..].iter().enumerate() {
            let entry = (i + 2) * 2;
            let cluster = u16::from_le_bytes(fat[entry..entry + 2].try_into().unwrap());
            ensure!(cluster == 0 || cluster == 0xfff7 || slot.is_some(), "Missing K750i data for an allocated FAT cluster");
        }
        volume.bytes = bytes;
        Ok(volume)
    }

    fn get(&self, i: usize) -> Option<&[u8]> {
        match self.slots[i]? {
            Slot::Nor(off) => Some(&self.nor[off..off + PAIR]),
            Slot::Nand(page) => Some(&self.main[page * SECTOR..(page + 2) * SECTOR]),
        }
    }

    pub(super) fn bytes_mut(&mut self) -> &mut [u8] { &mut self.bytes }

    pub(super) fn save(mut self, path: &Path) -> Result<()> {
        let mut free = std::mem::take(&mut self.free).into_iter();
        let mut nor_changed = false;
        let mut nand_changed = false;
        for i in 0..self.slots.len() {
            let data = &self.bytes[i * PAIR..(i + 1) * PAIR];
            let allocated = if i >= FIRST_DATA_PAIR {
                let entry = SECTOR + (i - FIRST_DATA_PAIR + 2) * 2;
                !matches!(u16::from_le_bytes(self.bytes[entry..entry + 2].try_into().unwrap()), 0 | 0xfff7)
            } else { true };
            if self.get(i).map_or(!allocated, |old| old == data) { continue; }
            match self.slots[i] {
                Some(Slot::Nor(off)) => {
                    self.nor[off..off + PAIR].copy_from_slice(data);
                    nor_changed = true;
                }
                slot => {
                    let page = match slot {
                        Some(Slot::Nand(page)) => page,
                        None => free.next().context("Not enough free K750i NAND slots for this copy")?,
                        Some(Slot::Nor(_)) => unreachable!(),
                    };
                    // A host write replaces the complete data in this saved image.
                    // Keep physical ordering, footer, and metadata used by the guest FTL.
                    self.main[page * SECTOR..(page + 2) * SECTOR].copy_from_slice(data);
                    let tag = 0xaa000000 | ((BASE + i) as u32);
                    self.spare[page * SPARE..page * SPARE + 4].copy_from_slice(&tag.to_le_bytes());
                    for (half, sector) in data.chunks_exact(SECTOR).enumerate() {
                        let offset = (page + half) * SPARE;
                        self.spare[offset + 6..offset + 9].copy_from_slice(&ecc(sector));
                        self.spare[offset + 9] = 0;
                    }
                    nand_changed = true;
                }
            }
        }
        if nor_changed { fs::write(path.join("fs.bin"), self.nor)?; }
        if nand_changed {
            fs::write(path.join("nand-main.bin"), self.main)?;
            fs::write(path.join("nand-spare.bin"), self.spare)?;
        }
        Ok(())
    }
}

fn record(slots: &mut [Option<Slot>], tag: u32, slot: Slot) -> Result<bool> {
    let id = (tag & 0xffffff) as usize;
    let Some(entry) = id.checked_sub(BASE).and_then(|i| slots.get_mut(i)) else { return Ok(false); };
    ensure!(entry.is_none(), "Ambiguous K750i flash mapping for block {id}");
    *entry = Some(slot);
    Ok(true)
}

fn erased(data: &[u8]) -> bool { data.iter().all(|byte| *byte == 0xff) }

fn validate_nand_pair(main: &[u8], spare: &[u8], page: usize) -> Result<()> {
    ensure!(erased(&spare[SPARE..SPARE + 6]) && erased(&spare[4..6]), "Invalid K750i NAND pair at page {page}");
    for half in 0..2 {
        let offset = half * SPARE;
        let sector = &main[(page + half) * SECTOR..(page + half + 1) * SECTOR];
        ensure!(spare[offset + 9] == 0 && spare[offset + 6..offset + 9] == ecc(sector),
            "Invalid K750i NAND error correction bytes at page {}", page + half);
    }
    Ok(())
}

fn ecc(sector: &[u8]) -> [u8; 3] {
    // Parity for each value of the twelve address bits of a 512-byte page.
    // The controller stores the nine byte-address pairs first.
    let mut sums = 0u32;
    for (address, &byte) in sector.iter().enumerate() {
        if byte.count_ones() & 1 != 0 {
            for bit in 0..9 { sums ^= 1 << (2 * bit + ((address >> bit) & 1)); }
        }
        for (mask, bit) in [(0x55u8, 18), (0xaa, 19), (0x33, 20), (0xcc, 21), (0x0f, 22), (0xf0, 23)] {
            sums ^= ((byte & mask).count_ones() & 1) << bit;
        }
    }
    let [a, b, c, _] = (!sums).to_le_bytes();
    [a, b, c]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nand_ecc_address_and_data_parity() {
        let mut sector = [0xff; SECTOR];
        assert_eq!(ecc(&sector), [0xff; 3]);
        sector[0] ^= 1;
        assert_eq!(ecc(&sector), [0xaa; 3]);
        sector[0] ^= 1;
        sector[511] ^= 0x80;
        assert_eq!(ecc(&sector), [0x55; 3]);
    }
}
