//! Examined OneC register protocol. Unknown reads/writes are explicit errors.
//! Reset shadows and instruction-based timing remain provisional; see docs.
use super::{display::Lcd, eeprom::I2c};
use std::collections::BTreeMap;

pub const BASE: u32 = 0x0400_0000;
pub const SIZE: u64 = 0x1_0000;

pub struct Soc {
    registers: BTreeMap<u32, u8>,
    pub lcd: Lcd,
    pub i2c: I2c,
    timer_counter: u16,
    system_counter: u32,
    system_pending: bool,
    transfer_count: u16,
    transfer_deadline: Option<u64>,
    rf_deadline: Option<(u64, u16)>,
    serial_deadline: Option<u64>,
    pub timer_divider: u64,
    pub system_ticks: u64,
    pub reset_transfers: u64,
    pub serial_transfers: u64,
}

impl Soc {
    pub fn new(eeprom: Option<Vec<u8>>) -> Self {
        let mut result = Self {
            registers: BTreeMap::new(),
            lcd: Lcd::default(),
            i2c: I2c::new(eeprom),
            timer_counter: 0,
            system_counter: 0,
            system_pending: false,
            transfer_count: 0,
            transfer_deadline: None,
            rf_deadline: None,
            serial_deadline: None,
            timer_divider: 12,
            system_ticks: 0,
            reset_transfers: 0,
            serial_transfers: 0,
        };
        // Examined cold-reset RMW/configuration registers, not invented ready bits.
        for address in [
            0x0002, 0x0006, 0x1408, 0x1e02, 0x1e26, 0x000e, 0x0016, 0x1a06, 0x0032, 0x1000, 0x0030,
            0x0020, 0x0022, 0x140a, 0x0050, 0x00a0, 0x00a2, 0x0200, 0x1e08, 0x1e06,
        ] {
            result.store(address, 2, 0);
        }
        for (start, end) in [(0x1020, 0x1028), (0x00c0, 0x00c8), (0x2080, 0x20c0)] {
            for address in start..end {
                result.registers.insert(address, 0);
            }
        }
        for address in [
            0x0100, 0x0104, 0x0110, 0x0112, 0x0114, 0x0116, 0x0118, 0x011a, 0x011c, 0x011e,
        ] {
            result.store(address, 2, 0);
        }
        result.store(0x0062, 2, 0x03ff); // No external keypad contacts.
        result
    }

    fn store(&mut self, offset: u32, size: usize, value: u64) {
        for i in 0..size {
            self.registers
                .insert(offset + i as u32, (value >> (i * 8)) as u8);
        }
    }

    fn word(&self, offset: u32) -> u16 {
        u16::from(self.registers.get(&offset).copied().unwrap_or(0))
            | (u16::from(self.registers.get(&(offset + 1)).copied().unwrap_or(0)) << 8)
    }

    pub fn read(&self, offset: u32, size: usize) -> Result<u64, String> {
        if !matches!(size, 1 | 2 | 4) {
            return Err(format!("Unsupported OneC read width {size}"));
        }
        if size == 2 {
            match offset {
                0x0034 => return Ok(0xff & !self.pending_sources()),
                // Radio/frame IRQ triggering is unresolved. The examined RTOS
                // timebase must not synthesize an unverified radio event.
                0x1004 => return Ok(0),
                0x1006 => return Ok(u64::from(self.system_pending)),
                0x1014 => return Ok(u64::from(self.timer_counter)),
                0xe018 => return Ok(u64::from(self.transfer_count)),
                0xe01a => return Ok(0), // Examined mode-zero transfer idle; other modes stop.
                0x0020 => return Ok(u64::from(self.i2c.control)),
                0x0022 => return Ok(u64::from(self.i2c.status)),
                _ => {}
            }
        }
        let mut value = 0u64;
        for i in 0..size {
            let byte = self.registers.get(&(offset + i as u32)).ok_or_else(|| {
                format!(
                    "Unimplemented N500 register read 0x{:08x}/{size}",
                    BASE + offset
                )
            })?;
            value |= u64::from(*byte) << (i * 8);
        }
        Ok(value)
    }

    pub fn write(&mut self, offset: u32, size: usize, value: u64, now: u64) -> Result<(), String> {
        if !matches!(size, 1 | 2 | 4) || !self.writable(offset, size) {
            return Err(format!(
                "Unimplemented N500 register write 0x{:08x}/{size}=0x{value:x}",
                BASE + offset
            ));
        }
        if offset == 0x0300 && size == 1 {
            return self.lcd.command(value as u8);
        }
        if offset == 0x0301 && size == 1 {
            self.lcd.data(value as u8);
            return Ok(());
        }
        self.store(offset, size, value);
        if size == 2 {
            match offset {
                0x0020 => self.i2c.command(value as u16)?,
                0x0038 if value & 0x10 != 0 => self.system_pending = false,
                // isr.o writes saved R2, including zero, before allowing nested
                // interrupts. This is a strobe, not write-one-to-clear.
                0x1006 => self.system_pending = false,
                0x1014 => self.timer_counter = value as u16,
                0xe002 if value != 0 => {
                    let mode = self.word(0xe000);
                    if mode != 0 {
                        return Err(format!("Unimplemented OneC transfer mode 0x{mode:x}; DSP transfer/execution required"));
                    }
                    self.transfer_count = value as u16;
                    self.transfer_deadline = Some(now + 16.max(value * 4));
                }
                // hal_rf.o requests bit7 and polls the acknowledgement at bit8.
                0x0012 => {
                    self.rf_deadline = Some((now + 16, if value & 0x80 != 0 { 0 } else { 0x100 }))
                }
                // hal_glob.o writes a transmit count and waits for it to drain.
                0x1236 if value != 0 => self.serial_deadline = Some(now + value * 32),
                _ => {}
            }
        }
        Ok(())
    }

    fn writable(&self, offset: u32, size: usize) -> bool {
        let end = offset + size as u32;
        [
            (0, 0x100),
            (0x100, 0x140),
            (0x200, 0x204),
            (0x300, 0x302),
            (0x1000, 0x10e0),
            (0x1200, 0x1240),
            (0x1400, 0x1420),
            (0x1a00, 0x1a20),
            (0x1e00, 0x1e28),
            (0x2000, 0x20c0),
            (0xe000, 0xe080),
            (0xf000, 0xf400),
            (0xff00, 0xff20),
        ]
        .iter()
        .any(|&(start, limit)| offset >= start && end <= limit)
    }

    pub fn advance(&mut self, now: u64) {
        if self
            .transfer_deadline
            .is_some_and(|deadline| now >= deadline)
        {
            self.transfer_count = 0;
            self.transfer_deadline = None;
            self.reset_transfers += 1;
        }
        if let Some((deadline, target)) = self.rf_deadline {
            if now >= deadline {
                self.store(0x0012, 2, u64::from((self.word(0x0012) & !0x100) | target));
                self.rf_deadline = None;
            }
        }
        if self.serial_deadline.is_some_and(|deadline| now >= deadline) {
            self.store(0x1236, 2, 0);
            self.serial_deadline = None;
            self.serial_transfers += 1;
        }
        if now % self.timer_divider == 0 {
            let control = self.word(0x1000);
            let period = self.word(0x1002);
            if control & 1 != 0 && period != 0 {
                self.timer_counter = self.timer_counter.wrapping_add(1);
                if self.timer_counter >= period {
                    self.timer_counter = 0;
                }
            }
            // The RTOS clock is a distinct interrupt (bit 4), acknowledged at
            // 0x1006. Its original handler advances 18,460 us: four GSM frames,
            // or 20,000 quarter-bit clocks. The instruction divider is a coarse
            // timing approximation; the fitted ARM clock is still unknown.
            if control & 2 != 0 {
                self.system_counter += 1;
                if self.system_counter == 20_000 {
                    self.system_counter = 0;
                    self.system_pending = true;
                    self.system_ticks += 1;
                }
            }
        }
    }

    fn pending_sources(&self) -> u64 {
        if self.system_pending {
            0x10
        } else {
            0
        }
    }

    pub fn irq_pending(&self) -> bool {
        self.pending_sources() & !u64::from(self.word(0x0030)) != 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn transfer_status_changes_with_time_and_unknown_reads_still_reject() {
        let mut soc = Soc::new(None);
        assert!(soc.read(0x8000, 2).is_err());
        soc.write(0xe000, 2, 0, 0).unwrap();
        soc.write(0xe002, 2, 1, 10).unwrap();
        assert_eq!(soc.read(0xe018, 2).unwrap(), 1);
        assert_eq!(soc.read(0xe018, 2).unwrap(), 1); // Polling does not complete it.
        soc.advance(25);
        assert_eq!(soc.read(0xe018, 2).unwrap(), 1);
        soc.advance(26);
        assert_eq!(soc.read(0xe018, 2).unwrap(), 0);
        soc.write(0xe000, 2, 1, 30).unwrap();
        assert!(soc.write(0xe002, 2, 1, 30).is_err());
        assert!(soc.write(0x8000, 2, 1, 30).is_err());
    }
    #[test]
    fn rtos_clock_is_distinct_from_unimplemented_radio_events() {
        let mut soc = Soc::new(None);
        soc.timer_divider = 1;
        soc.write(0x1000, 2, 3, 0).unwrap();
        soc.write(0x1002, 2, 2, 0).unwrap();
        for now in 1..20_000 {
            soc.advance(now);
        }
        assert!(!soc.irq_pending());
        assert_eq!(soc.read(0x1004, 2).unwrap(), 0);
        soc.advance(20_000);
        assert!(soc.irq_pending());
        assert_eq!(soc.read(0x0034, 2).unwrap(), 0xef);
        soc.write(0x0030, 2, 0x10, 20_000).unwrap();
        assert!(!soc.irq_pending());
        soc.write(0x0030, 2, 0, 20_000).unwrap();
        assert!(soc.irq_pending());
        soc.write(0x1006, 2, 0, 20_000).unwrap();
        assert!(!soc.irq_pending());
        for now in 20_001..=40_000 {
            soc.advance(now);
        }
        assert!(soc.irq_pending());
        soc.write(0x0038, 2, 1, 40_000).unwrap();
        assert!(soc.irq_pending());
        soc.write(0x0038, 2, 0x10, 40_000).unwrap();
        assert!(!soc.irq_pending());
        soc.write(0x1000, 2, 1, 40_000).unwrap();
        for now in 40_001..=60_000 {
            soc.advance(now);
        }
        assert!(!soc.irq_pending());
        assert_eq!(soc.system_ticks, 2);
    }
}
