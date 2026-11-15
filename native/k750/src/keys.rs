// SPDX-License-Identifier: GPL-2.0-or-later
//! Host vocabulary and ideal electrical contacts, adapted from db2010-keypad.c.
use anyhow::{ensure, Result};
use e830::PhoneKey;
use serde::Serialize;

#[derive(Clone, Copy, Debug, Serialize)]
pub struct KeyBinding {
    pub label: &'static str,
    pub mask: u32,
}

pub fn qmp_contacts(mask: u32) -> Vec<&'static str> {
    let keys = [("soft-left", PhoneKey::SoftLeft.bit()), ("soft-right", PhoneKey::SoftRight.bit()),
        ("up", PhoneKey::Up.bit()), ("down", PhoneKey::Down.bit()),
        ("left", PhoneKey::Left.bit()), ("right", PhoneKey::Right.bit()),
        ("select", PhoneKey::Ok.bit()), ("back", PhoneKey::Call.bit()), ("clear", PhoneKey::End.bit()),
        ("1", PhoneKey::One.bit()), ("2", PhoneKey::Two.bit()), ("3", PhoneKey::Three.bit()),
        ("4", PhoneKey::Four.bit()), ("5", PhoneKey::Five.bit()), ("6", PhoneKey::Six.bit()),
        ("7", PhoneKey::Seven.bit()), ("8", PhoneKey::Eight.bit()), ("9", PhoneKey::Nine.bit()),
        ("0", PhoneKey::Zero.bit()), ("star", PhoneKey::Star.bit()), ("hash", PhoneKey::Hash.bit()),
        ("power",1<<21), ("volume-up",1<<22), ("volume-down",1<<23),
        ("camera-focus",1<<24), ("camera",1<<25), ("walkman",1<<26), ("play-pause",1<<27)];
    keys.into_iter().filter_map(|(name, bit)| (mask & bit != 0).then_some(name)).collect()
}

pub fn bindings(device: &str) -> Vec<KeyBinding> {
    if device == "n500" { return vec![]; }
    let mut keys = vec![
        KeyBinding { label: "Left soft", mask: PhoneKey::SoftLeft.bit() },
        KeyBinding { label: "Up", mask: PhoneKey::Up.bit() },
        KeyBinding { label: "Right soft", mask: PhoneKey::SoftRight.bit() },
        KeyBinding { label: "Left", mask: PhoneKey::Left.bit() },
        KeyBinding { label: "OK", mask: PhoneKey::Ok.bit() },
        KeyBinding { label: "Right", mask: PhoneKey::Right.bit() },
        KeyBinding { label: if device == "k750i" { "Back" } else { "Call" }, mask: PhoneKey::Call.bit() },
        KeyBinding { label: "Down", mask: PhoneKey::Down.bit() },
        KeyBinding { label: if device == "k750i" { "C" } else { "End" }, mask: PhoneKey::End.bit() },
    ];
    for (label, key) in [("1", PhoneKey::One), ("2 abc", PhoneKey::Two), ("3 def", PhoneKey::Three),
        ("4 ghi", PhoneKey::Four), ("5 jkl", PhoneKey::Five), ("6 mno", PhoneKey::Six),
        ("7 pqrs", PhoneKey::Seven), ("8 tuv", PhoneKey::Eight), ("9 wxyz", PhoneKey::Nine),
        ("*", PhoneKey::Star), ("0", PhoneKey::Zero), ("#", PhoneKey::Hash)] {
        keys.push(KeyBinding { label, mask: key.bit() });
    }
    if device == "k750i" {
        for (label, bit) in [("Power", 21), ("Volume +", 22), ("Volume −", 23),
            ("Focus", 24), ("Camera", 25), ("Activity", 26), ("Play/pause", 27)] {
            keys.push(KeyBinding { label, mask: 1 << bit });
        }
    }
    keys
}

#[derive(Default)]
pub struct Keypad {
    pub pressed: u32,
    control: Option<u16>,
    rows: u8,
    pub pending: bool,
    pub transitions: u64,
}

impl Keypad {
    pub fn set(&mut self, mask: u32) -> Result<()> {
        ensure!(mask & !0x0fff_ffff == 0, "Unknown K750i key bits");
        if mask != self.pressed {
            self.pressed = mask;
            self.pending = true;
            self.transitions += 1;
        }
        Ok(())
    }

    fn columns(&self) -> u8 {
        let contacts = [
            (PhoneKey::One.bit(), 3, 1), (PhoneKey::Two.bit(), 2, 1), (PhoneKey::Three.bit(), 0, 1),
            (PhoneKey::Four.bit(), 3, 2), (PhoneKey::Five.bit(), 2, 2), (PhoneKey::Six.bit(), 0, 2),
            (PhoneKey::Seven.bit(), 3, 4), (PhoneKey::Eight.bit(), 2, 4), (PhoneKey::Nine.bit(), 0, 4),
            (PhoneKey::Star.bit(), 3, 8), (PhoneKey::Zero.bit(), 2, 8), (PhoneKey::Hash.bit(), 0, 8),
            (PhoneKey::SoftLeft.bit(), 1, 1), (PhoneKey::SoftRight.bit(), 1, 2),
            (PhoneKey::Call.bit(), 1, 4), (PhoneKey::End.bit(), 1, 8),
            (PhoneKey::Up.bit(), 4, 0x1a), (PhoneKey::Right.bit(), 4, 0x19),
            (PhoneKey::Down.bit(), 4, 0x0d), (PhoneKey::Left.bit(), 4, 0x0e), (PhoneKey::Ok.bit(), 4, 8),
            (1 << 21, 6, 1), (1 << 22, 3, 0x10), (1 << 23, 2, 0x10),
            (1 << 24, 5, 4), (1 << 25, 5, 0x0c), (1 << 26, 5, 2), (1 << 27, 1, 0x10),
        ];
        contacts.into_iter().fold(0xff, |columns, (mask, row, bits)| {
            if self.pressed & mask != 0 && (row == 6 || self.rows & (1 << row) == 0) { columns & !bits } else { columns }
        })
    }

    pub fn read(&self, address: u32, size: usize) -> Result<u64> {
        ensure!(size == 2, "keypad_access_unmodeled");
        match address {
            0xfb01_0000 => self.control.map(u64::from).ok_or_else(|| anyhow::anyhow!("keypad_control_unknown")),
            0xfb01_0002 => Ok(u64::from(self.rows) | (u64::from(self.columns()) << 6)),
            _ => anyhow::bail!("keypad_access_unmodeled"),
        }
    }

    pub fn write(&mut self, address: u32, size: usize, value: u64) -> Result<()> {
        ensure!(size == 2, "keypad_access_unmodeled");
        match address {
            0xfb01_0000 => {
                ensure!(matches!(value, 0x121 | 0x1a1 | 0x321 | 0x3a1), "keypad_control_write_unmodeled");
                self.control = Some(value as u16);
                if value & 0x100 != 0 { self.pending = false; }
            }
            0xfb01_0002 => {
                let rows = value & 63;
                ensure!(value & !0x3fff == 0 && matches!(rows, 63 | 62 | 61 | 59 | 55 | 47 | 31), "keypad_rows_write_unmodeled");
                self.rows = rows as u8;
            }
            _ => anyhow::bail!("keypad_access_unmodeled"),
        }
        Ok(())
    }

    pub fn irq(&self) -> bool { self.pending && self.control.is_some_and(|v| v & 0x280 == 0x280) }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn contacts_are_active_low_and_releases_preserve_other_contacts() {
        let mut keypad = Keypad::default();
        keypad.write(0xfb010002, 2, 55).unwrap(); // Select row three.
        keypad.set(PhoneKey::One.bit() | PhoneKey::Four.bit()).unwrap();
        assert_eq!((keypad.read(0xfb010002, 2).unwrap() >> 6) & 3, 0);
        keypad.set(PhoneKey::Four.bit()).unwrap();
        assert_eq!((keypad.read(0xfb010002, 2).unwrap() >> 6) & 3, 1);
        keypad.set(0).unwrap();
        assert_eq!((keypad.read(0xfb010002, 2).unwrap() >> 6) & 3, 3);
        keypad.write(0xfb010000, 2, 0x3a1).unwrap();
        assert!(!keypad.irq());
        keypad.set(1 << 21).unwrap();
        keypad.write(0xfb010002, 2, 63).unwrap();
        assert_eq!((keypad.read(0xfb010002, 2).unwrap() >> 6) & 1, 0);
        assert!(keypad.irq());
        assert!(keypad.write(0xfb010002, 1, 0).is_err());
    }
}

