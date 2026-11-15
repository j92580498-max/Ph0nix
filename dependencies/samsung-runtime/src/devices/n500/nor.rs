//! Sharp/Intel-style NOR cycles used by N500's original flash drivers.
//! Cells are private writable copies; host firmware files are never overwritten.
use super::memory::NOR_SIZE;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Array,
    Status,
    Id,
}
#[derive(Clone, Copy)]
enum Pending {
    Program,
    Erase(usize),
    Lock,
}

pub struct Nor {
    pub cells: Vec<u8>,
    mode: Mode,
    pending: Option<Pending>,
    status: u16,
    pub commands: u64,
    pub programs: u64,
    pub erases: u64,
}

impl Nor {
    pub fn is_array(&self) -> bool {
        matches!(self.mode, Mode::Array)
    }

    /// Materialize the bus view for Unicorn's executable memory mapping.
    /// ID reads outside the implemented words still reject in `read`.
    pub fn mapped_view(&self, offset: usize, size: usize) -> Vec<u8> {
        match self.mode {
            Mode::Array => self.cells[offset..offset + size].to_vec(),
            Mode::Status => (0..size)
                .map(|i| self.status.to_le_bytes()[(offset + i) & 1])
                .collect(),
            Mode::Id => {
                let id = [0xb0, 0, 0xe1, 0];
                (0..size)
                    .map(|i| id.get(offset + i).copied().unwrap_or(0xff))
                    .collect()
            }
        }
    }

    pub fn erased() -> Self {
        Self {
            cells: vec![0xff; NOR_SIZE],
            mode: Mode::Array,
            pending: None,
            status: 0x80,
            commands: 0,
            programs: 0,
            erases: 0,
        }
    }

    pub fn read(&self, offset: usize, size: usize) -> Result<Vec<u8>, String> {
        if size == 0
            || offset
                .checked_add(size)
                .is_none_or(|end| end > self.cells.len())
        {
            return Err("N500 NOR access exceeds its 2 MiB bank".into());
        }
        match self.mode {
            Mode::Array => Ok(self.cells[offset..offset + size].to_vec()),
            Mode::Status => Ok((0..size)
                .map(|i| self.status.to_le_bytes()[(offset + i) & 1])
                .collect()),
            Mode::Id => {
                // N500 flash.o:0x4487a accepts Sharp 0x00b0/0x00e1.
                let id = [0xb0, 0, 0xe1, 0];
                if offset + size > id.len() {
                    return Err("Unimplemented N500 NOR identification offset".into());
                }
                Ok(id[offset..offset + size].to_vec())
            }
        }
    }

    /// Return the affected cell range so executable mappings can be synchronized.
    pub fn write(
        &mut self,
        offset: usize,
        size: usize,
        value: u64,
    ) -> Result<(usize, usize), String> {
        let previous = self.mode;
        let changed = self.write_cycle(offset, size, value)?;
        Ok(if previous != self.mode {
            (0, NOR_SIZE)
        } else {
            changed
        })
    }

    fn write_cycle(
        &mut self,
        offset: usize,
        size: usize,
        value: u64,
    ) -> Result<(usize, usize), String> {
        if size != 2 || offset & 1 != 0 || offset + size > self.cells.len() {
            return Err(format!(
                "N500 NOR expects an aligned halfword cycle at 0x{offset:x}/{size}"
            ));
        }
        let value = value as u16;
        self.commands += 1;
        if let Some(pending) = self.pending.take() {
            match pending {
                Pending::Program => {
                    let old =
                        u16::from_le_bytes(self.cells[offset..offset + 2].try_into().unwrap());
                    self.cells[offset..offset + 2].copy_from_slice(&(old & value).to_le_bytes());
                    self.status = if old & value == value { 0x80 } else { 0x90 };
                    self.mode = Mode::Status;
                    self.programs += 1;
                }
                Pending::Erase(target) if target == offset && value == 0xd0 => {
                    // Bottom boot-block layout selected by the original Sharp driver.
                    let size = if offset < 0x10000 { 0x2000 } else { 0x10000 };
                    let start = offset & !(size - 1);
                    self.cells[start..start + size].fill(0xff);
                    self.status = 0x80;
                    self.mode = Mode::Status;
                    self.erases += 1;
                    return Ok((start, size));
                }
                Pending::Lock if matches!(value, 1 | 0xd0 | 0xf1) => {
                    self.status = 0x80;
                }
                _ => {
                    return Err(format!(
                        "Unimplemented N500 NOR command continuation 0x{value:04x}"
                    ))
                }
            }
            return Ok((offset, size));
        }
        match value {
            0xff | 0xf0 => self.mode = Mode::Array,
            0x70 => self.mode = Mode::Status,
            0x90 => self.mode = Mode::Id,
            0x50 => self.status = 0x80,
            0x40 | 0x10 => self.pending = Some(Pending::Program),
            0x20 => self.pending = Some(Pending::Erase(offset)),
            0x60 => self.pending = Some(Pending::Lock),
            _ => {
                return Err(format!(
                    "Unimplemented N500 NOR cycle 0x{value:04x} at 0x{offset:x}"
                ))
            }
        }
        Ok((offset, size))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reset_unlock_and_identification_do_not_overwrite_executable_cells() {
        let mut nor = Nor::erased();
        nor.cells[..4].copy_from_slice(&[0x06, 0, 0, 0xea]);
        for value in [0xff, 0x60, 1, 0x70, 0xff, 0x60, 0xf1, 0x70, 0xff] {
            nor.write(0, 2, value).unwrap();
        }
        assert_eq!(nor.read(0, 4).unwrap(), [6, 0, 0, 0xea]);
        nor.write(0, 2, 0x90).unwrap();
        assert_eq!(nor.read(0, 4).unwrap(), [0xb0, 0, 0xe1, 0]);
        nor.write(0, 2, 0xff).unwrap();
        assert_eq!(&nor.cells[..4], &[6, 0, 0, 0xea]);
    }
    #[test]
    fn programming_and_erase_change_only_the_addressed_cells() {
        let mut nor = Nor::erased();
        nor.write(0x10000, 2, 0x40).unwrap();
        nor.write(0x10000, 2, 0x1234).unwrap();
        assert_eq!(&nor.cells[0x10000..0x10002], &[0x34, 0x12]);
        nor.write(0x10000, 2, 0x40).unwrap();
        nor.write(0x10000, 2, 0xffff).unwrap();
        assert_eq!(nor.read(0, 2).unwrap(), [0x90, 0]);
        nor.cells[0x100] = 0;
        nor.write(0x10000, 2, 0x20).unwrap();
        nor.write(0x10000, 2, 0xd0).unwrap();
        assert_eq!(&nor.cells[0x10000..0x10002], &[0xff, 0xff]);
        assert_eq!(nor.cells[0x100], 0);
        assert!(nor.write(1, 2, 0xff).is_err());
    }
    #[test]
    fn status_and_array_changes_synchronize_the_entire_executable_bus_view() {
        let mut nor = Nor::erased();
        nor.cells[0x100..0x104].copy_from_slice(&[1, 2, 3, 4]);
        assert_eq!(nor.write(0, 2, 0x70).unwrap(), (0, NOR_SIZE));
        assert_eq!(nor.mapped_view(0x100, 4), [0x80, 0, 0x80, 0]);
        assert_eq!(nor.write(0, 2, 0xff).unwrap(), (0, NOR_SIZE));
        assert_eq!(nor.mapped_view(0x100, 4), [1, 2, 3, 4]);
    }
}
