// SPDX-License-Identifier: GPL-2.0-or-later
//! Exact-width latch, memory decoder and section-table contracts from DB2010.
use anyhow::{ensure, Context, Result};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Deserialize, Clone)]
struct LatchSpec {
    address: u32, width: usize, initial_value: Option<u32>, read_or_mask: u32,
    #[serde(default)] access: String,
    #[serde(default)] allowed_writes: Option<Vec<u32>>,
    #[serde(default)] f500_lowbyte_alias: bool,
    #[serde(default)] rtc_halfword_alias: bool,
}
struct Latch { spec: LatchSpec, value: u32, known: u32 }
#[derive(Deserialize)]
struct Specs { device_registers: Vec<LatchSpec> }

pub struct Registers {
    latches: BTreeMap<u32, Latch>,
    pub controller: [Option<u32>; 11],
    pub regions: [u32; 24],
    pub table_dirty: bool,
    power: u16,
    startup: u8,
    pub uart: [Vec<u8>; 4],
}

const UART_BASES: [u32; 4] = [0xfb06_0000, 0xfc04_0000, 0xfc03_0000, 0xfb04_0000];
fn mask(size: usize) -> u32 { if size == 4 { u32::MAX } else { (1u32 << (size * 8)) - 1 } }

impl Registers {
    pub fn new() -> Result<Self> {
        let specs: Specs = serde_json::from_str(include_str!("../resources/registers.json"))?;
        let mut latches = BTreeMap::new();
        for spec in specs.device_registers {
            ensure!(matches!(spec.width, 1 | 2 | 4), "Invalid register width");
            let value = spec.initial_value.unwrap_or(0);
            let known = if spec.initial_value.is_some() { mask(spec.width) } else { 0 };
            ensure!(latches.insert(spec.address, Latch { spec, value, known }).is_none(), "Duplicate register address");
        }
        let mut controller = [None; 11];
        controller[10] = Some(0x0401_8060);
        Ok(Self { latches, controller, regions: [0; 24], table_dirty: false, power: 0, startup: 0,
            uart: std::array::from_fn(|_| Vec::new()) })
    }

    pub fn pages(&self) -> BTreeSet<u32> {
        self.latches.keys().map(|address| address & !0xfff)
            .chain([0x1400_0000, 0xfe00_4000, 0xf905_0000, 0xfb01_0000, 0xf901_0000])
            .chain(UART_BASES).collect()
    }

    fn latch(&self, address: u32, size: usize) -> Result<&Latch> {
        let latch = self.latches.get(&address).context("device_register_access_unmodeled")?;
        ensure!(size == latch.spec.width || (size == 1 && latch.spec.f500_lowbyte_alias)
            || (size == 2 && latch.spec.rtc_halfword_alias), "device_register_access_unmodeled");
        Ok(latch)
    }

    pub fn read(&self, address: u32, size: usize) -> Result<u64> {
        ensure!(matches!(size, 1 | 2 | 4), "device_register_access_unmodeled");
        if let Some(index) = controller_index(address, size)? {
            return self.controller[index].map(u64::from).context("controller_latch_unknown_word");
        }
        if let Some(index) = region_index(address, size)? { return Ok(u64::from(self.regions[index])); }
        if address == 0xf905_0058 {
            ensure!(size == 2, "power_hold_access_unmodeled");
            return Ok(u64::from(self.power));
        }
        if address == 0xf905_005c {
            ensure!(size == 2, "startup_status_access_unmodeled");
            return Ok(u64::from(self.startup & 5));
        }
        if UART_BASES.iter().any(|base| address == base + 0x2c) {
            ensure!(size == 2, "uart_access_unmodeled");
            return Ok(1); // Inherited polled-TX immediate-drain assumption.
        }
        let latch = self.latch(address, size)?;
        ensure!(latch.spec.access != "write-only", "device_register_read_unmodeled");
        let required = mask(if latch.spec.f500_lowbyte_alias { size } else { latch.spec.width });
        ensure!(latch.known & required == required, "device_register_initial_state_unknown");
        let mut value = latch.value | latch.spec.read_or_mask;
        if address == 0xf900_000a {
            let direction = self.latches.get(&0xf900_000c).context("serial_gpio_direction_unknown")?;
            ensure!(direction.known == mask(direction.spec.width), "serial_gpio_direction_unknown");
            let inputs = 0x11 & !direction.value;
            value = (value & !inputs) | (0x11 & inputs);
        }
        if address == 0xf900_0028 {
            let direction = self.latches.get(&0xf900_002a).context("memorystick_detect_direction_unknown")?;
            ensure!(direction.known == mask(direction.spec.width), "memorystick_detect_direction_unknown");
            if direction.value & 4 == 0 { value |= 4; }
        }
        Ok(u64::from(value & mask(size)))
    }

    pub fn write(&mut self, address: u32, size: usize, value: u64) -> Result<()> {
        ensure!(matches!(size, 1 | 2 | 4) && value <= u64::from(mask(size)), "device_register_access_unmodeled");
        if let Some(index) = controller_index(address, size)? {
            ensure!(index != 10 || value == 0x0401_8060, "bank_decoder_change_unmodeled");
            self.controller[index] = Some(value as u32);
            return Ok(());
        }
        if let Some(index) = region_index(address, size)? {
            self.regions[index] = value as u32;
            self.table_dirty = true;
            return Ok(());
        }
        if address == 0xf905_0058 {
            ensure!(size == 2, "power_hold_access_unmodeled");
            ensure!(value as u16 & !0x8d == self.power & !0x8d, "power_hold_other_bits_unmodeled");
            ensure!(self.power & 1 == 0 || value & 1 != 0, "power_hold_release_unmodeled");
            self.power = value as u16;
            return Ok(());
        }
        if address == 0xf905_005c {
            ensure!(size == 1 && value == 0, "startup_status_write_unmodeled");
            self.startup = 0;
            return Ok(());
        }
        if let Some(port) = UART_BASES.iter().position(|base| address == base + 2) {
            ensure!(size == 2 && value <= 255, "uart_access_unmodeled");
            ensure!(self.uart[port].len() < 1024 * 1024, "UART capture limit reached");
            self.uart[port].push(value as u8);
            return Ok(());
        }
        let latch = self.latch(address, size)?;
        ensure!(latch.spec.access != "read-only", "device_register_write_unmodeled");
        ensure!(!latch.spec.rtc_halfword_alias || size != 2 || value <= 255, "rtc_marker_upper_byte_unmodeled");
        if let Some(allowed) = &latch.spec.allowed_writes {
            ensure!(allowed.contains(&(value as u32)), "device_register_command_unmodeled");
        }
        let latch = self.latches.get_mut(&address).unwrap();
        if latch.spec.f500_lowbyte_alias && size == 1 {
            latch.value = (latch.value & 0xff00) | value as u32;
            latch.known |= 255;
        } else {
            latch.value = value as u32 & mask(latch.spec.width);
            latch.known = mask(latch.spec.width);
        }
        Ok(())
    }

    pub fn section_table(&self) -> Result<Vec<u8>> {
        let mut words = [0u32; 4096];
        let mut occupied = [false; 4096];
        for index in 0..12 {
            let range = self.regions[index];
            if range & 0x8000_0000 == 0 { continue; }
            let attr = self.regions[index + 12];
            let start = (range & 0xfff) as usize;
            let end = ((range >> 16) & 0xfff) as usize;
            ensure!(range & 0x7000_f000 == 0 && attr & !0xdff == 0 && attr & 3 == 2 && attr & 0x10 != 0, "section_table_encoding_unmodeled");
            ensure!(start <= end, "section_table_reversed_range");
            for section in start..=end {
                ensure!(!occupied[section], "section_table_overlap_unmodeled");
                occupied[section] = true;
                words[section] = (section as u32) << 20 | attr;
            }
        }
        Ok(words.into_iter().flat_map(u32::to_le_bytes).collect())
    }

    pub fn established(&self) -> BTreeMap<String, Option<u32>> {
        self.latches.iter().map(|(address, latch)| (format!("0x{address:08x}"),
            (latch.known == mask(latch.spec.width)).then_some(latch.value))).collect()
    }
}

fn controller_index(address: u32, size: usize) -> Result<Option<usize>> {
    if address & !0xfff != 0x1400_0000 { return Ok(None); }
    let offset = address & 0xfff;
    ensure!(size == 4 && offset & 3 == 0 && (offset == 0 || (8..=0x28).contains(&offset)), "controller_latch_access_unmodeled");
    Ok(Some((offset / 4) as usize))
}
fn region_index(address: u32, size: usize) -> Result<Option<usize>> {
    if address & !0xfff != 0xfe00_4000 { return Ok(None); }
    let offset = address & 0xfff;
    ensure!(size == 4 && offset & 3 == 0 && offset < 96, "region_register_access_unmodeled");
    Ok(Some((offset / 4) as usize))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unknown_reads_bad_widths_and_rejected_commands_leave_latches_unchanged() {
        let mut r = Registers::new().unwrap();
        assert!(r.read(0x14000000, 4).is_err());
        assert!(r.write(0x14000004, 4, 123).is_err());
        assert!(r.write(0x14000028, 4, 1).is_err());
        assert_eq!(r.read(0x14000028, 4).unwrap(), 0x04018060);
        r.write(0x14000000, 4, 0x1234).unwrap();
        assert_eq!(r.read(0x14000000, 4).unwrap(), 0x1234);
        assert!(r.read(0x14000000, 2).is_err());
        assert!(r.write(0xf9010010, 4, 0xab).is_err());
        assert!(r.read(0xf9010010, 4).is_err());
        r.write(0xf9010010, 4, 0xaa).unwrap();
        assert!(r.read(0xf9010010, 4).is_err()); // Write-only service key.
    }
    #[test]
    fn generated_sections_preserve_permissions_and_reject_overlap_before_publication() {
        let mut r = Registers::new().unwrap();
        r.write(0xfe004000, 4, 0x84400440).unwrap();
        r.write(0xfe004030, 4, 0xc12).unwrap();
        let table = r.section_table().unwrap();
        assert_eq!(&table[0x440 * 4..0x440 * 4 + 4], &0x44000c12u32.to_le_bytes());
        assert_eq!(&table[0x439 * 4..0x439 * 4 + 4], &[0; 4]);
        r.write(0xfe004004, 4, 0x84400440).unwrap();
        r.write(0xfe004034, 4, 0xc12).unwrap();
        assert!(r.section_table().is_err());
    }
}
