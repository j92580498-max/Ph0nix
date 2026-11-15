//! S524A40X-style 32 KiB serial EEPROM and examined OneC I2C commands.
//! Address 0x53 and 16-bit offsets are established by the original driver.
use super::memory::EEPROM_SIZE;

pub struct I2c {
    cells: Vec<u8>,
    selected: bool,
    reading: bool,
    address_bytes: Vec<u8>,
    pointer: usize,
    pub control: u16,
    pub status: u16,
    pub transactions: u64,
    pub reads: u64,
    pub writes: u64,
}

impl I2c {
    pub fn new(cells: Option<Vec<u8>>) -> Self {
        Self {
            cells: cells.unwrap_or_else(|| vec![0xff; EEPROM_SIZE]),
            selected: false,
            reading: false,
            address_bytes: Vec::with_capacity(2),
            pointer: 0,
            control: 0,
            status: 0,
            transactions: 0,
            reads: 0,
            writes: 0,
        }
    }

    pub fn command(&mut self, value: u16) -> Result<(), String> {
        self.control = value;
        let command = (value >> 5) & 7;
        let byte = (value >> 8) as u8;
        let mut ack = false;
        self.status = match command {
            1 => {
                // STOP
                self.selected = false;
                0
            }
            2 => {
                // START/address, including repeated START
                self.selected = byte >> 1 == 0x53;
                self.reading = byte & 1 != 0;
                if self.selected && !self.reading {
                    self.address_bytes.clear();
                }
                ack = self.selected;
                self.transactions += 1;
                0x0400
            }
            0 => {
                // Transmit offset/data
                if self.selected && !self.reading {
                    ack = true;
                    if self.address_bytes.len() < 2 {
                        self.address_bytes.push(byte);
                        if self.address_bytes.len() == 2 {
                            self.pointer =
                                u16::from_be_bytes(self.address_bytes[..2].try_into().unwrap())
                                    as usize
                                    % self.cells.len();
                        }
                    } else {
                        self.cells[self.pointer] = byte;
                        self.pointer = (self.pointer + 1) % self.cells.len();
                        self.writes += 1;
                    }
                }
                0x0400
            }
            4 | 5 => {
                // Receive with ACK / final receive with NACK (i2c.o:0x13bce)
                if !self.selected || !self.reading {
                    return Err("N500 I2C receive has no addressed EEPROM read transaction".into());
                }
                self.control = (value & 0x00ff) | (u16::from(self.cells[self.pointer]) << 8);
                self.pointer = (self.pointer + 1) % self.cells.len();
                self.reads += 1;
                ack = true;
                0x0200
            }
            _ => return Err(format!("Unimplemented N500 I2C command {command}")),
        };
        if ack {
            self.status |= 0x0800;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn straps_addressing_repeated_start_and_final_receive_use_real_cells() {
        let mut bus = I2c::new(None);
        bus.command(0xa040).unwrap();
        assert_eq!(bus.status & 0x800, 0);
        bus.command(0xa640).unwrap();
        assert_ne!(bus.status & 0x800, 0);
        bus.command(0x1200).unwrap();
        bus.command(0x3400).unwrap();
        bus.command(0x5600).unwrap();
        bus.command(0x0020).unwrap();
        bus.command(0xa640).unwrap();
        bus.command(0x1200).unwrap();
        bus.command(0x3400).unwrap();
        bus.command(0xa740).unwrap();
        bus.command(0x0080).unwrap();
        assert_eq!(bus.control >> 8, 0x56);
        bus.command(0x00a0).unwrap();
        assert_eq!(bus.control >> 8, 0xff);
        bus.command(0x0020).unwrap();
        assert!(bus.command(0x00a0).is_err());
        assert_eq!((bus.reads, bus.writes), (2, 1));
    }
}
