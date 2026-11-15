//! E830 keyboard controller and physical contact matrix.
use std::collections::VecDeque;
use crate::devices::keys::PhoneKey;

fn coordinates(key: PhoneKey) -> u16 {
    let (row, column) = match key {
        PhoneKey::One => (1, 8), PhoneKey::Two => (2, 16), PhoneKey::Three => (2, 8),
        PhoneKey::Four => (1, 4), PhoneKey::Five => (1, 16), PhoneKey::Six => (2, 4),
        PhoneKey::Seven => (1, 2), PhoneKey::Eight => (4, 2), PhoneKey::Nine => (2, 2),
        PhoneKey::Zero => (2, 1), PhoneKey::Star => (1, 1), PhoneKey::Hash => (4, 1),
        PhoneKey::SoftLeft => (8, 4), PhoneKey::SoftRight => (8, 8),
        PhoneKey::Call => (8, 2), PhoneKey::End => (16, 2),
        // Main handset matrix: HKBD keys 4/5/6/7/13, consumed by the
        // native MMI list and select handlers. The extra records
        // 0x14..0x18 are not the handset's menu controls.
        PhoneKey::Up => (4, 8), PhoneKey::Down => (8, 16), PhoneKey::Left => (16, 16),
        PhoneKey::Right => (4, 16), PhoneKey::Ok => (16, 8),
    };
    row | column << 5
}

#[derive(Default)]
pub(crate) struct Keypad {
    control: u8,
    held: u32,
    events: VecDeque<(u8, u16)>,
    overflow: bool,
}

impl Keypad {
    pub fn set_keys(&mut self, keys: u32) {
        let keys = keys & ((1 << PhoneKey::ALL.len()) - 1);
        for key in PhoneKey::ALL {
            if (self.held ^ keys) & key.bit() != 0 {
                self.held ^= key.bit();
                // The ISR reports the currently asserted matrix, not the
                // released key. Callback 0x2118bf80 treats the old coordinate
                // on release as still held and suppresses HKBD_KEY_RELEASED.
                let coordinates = if keys & key.bit() != 0 {
                    coordinates(key)
                } else {
                    PhoneKey::ALL.into_iter().find(|other| self.held & other.bit() != 0)
                        .map_or(0, coordinates)
                };
                if self.events.len() < 64 {
                    self.events.push_back((if keys & key.bit() != 0 { 1 } else { 2 }, coordinates));
                } else { self.overflow = true; }
            }
        }
        self.held = keys;
    }

    pub fn irq_pending(&self) -> bool {
        self.events.front().is_some_and(|(event, _)| self.control & event != 0)
            || self.overflow && self.control & 4 != 0
    }

    pub fn read(&self, offset: u32, size: usize) -> u64 {
        let raw = match offset & !3 {
            0 => u32::from(self.control),
            4 => self.events.front().map_or(0, |(_, data)| u32::from(*data)),
            // ISR checks 0=asserted and writes ones to acknowledge.
            8 => 7 & !self.events.front().map_or(0, |(event, _)| u32::from(*event))
                & if self.overflow { !4 } else { u32::MAX },
            _ => 0,
        };
        (u64::from(raw) >> ((offset & 3) * 8)) & match size { 1 => 0xff, 2 => 0xffff, _ => 0xffff_ffff }
    }

    pub fn write(&mut self, offset: u32, value: u64) {
        match offset {
            0 => {
                self.control = value as u8;
                if self.control == 0x18 { self.events.clear(); self.overflow = false; }
            }
            8 => {
                if self.events.front().is_some_and(|(event, _)| value as u8 & event != 0) {
                    self.events.pop_front();
                }
                if value & 4 != 0 { self.overflow = false; }
            }
            _ => {} // key data is external input, not writable RAM
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn press_release_are_latched_masked_and_acknowledged_once() {
        let mut keypad = Keypad::default();
        assert_eq!(keypad.read(8, 1), 7);
        keypad.set_keys(PhoneKey::One.bit());
        assert!(!keypad.irq_pending());
        keypad.write(0, 3);
        assert!(keypad.irq_pending());
        assert_eq!(keypad.read(4, 2), 1 | 8 << 5);
        assert_eq!(keypad.read(8, 1), 6);
        keypad.set_keys(PhoneKey::One.bit());
        keypad.set_keys(0);
        assert_eq!(keypad.read(8, 1), 6);
        keypad.write(8, 2);
        assert_eq!(keypad.read(8, 1), 6);
        keypad.write(8, 7);
        assert_eq!(keypad.read(8, 1), 5);
        keypad.write(8, 7);
        assert!(!keypad.irq_pending());
        assert_eq!(keypad.read(8, 1), 7);
    }

    #[test]
    fn multiple_keys_release_and_read_only_coordinates() {
        let mut keypad = Keypad::default();
        keypad.write(0, 3);
        keypad.set_keys(PhoneKey::One.bit() | PhoneKey::Ok.bit());
        keypad.write(4, 0xffff);
        assert_eq!(keypad.read(4, 1), (1 | 8 << 5) & 255);
        assert_eq!(keypad.read(5, 1), 1);
        keypad.write(8, 7);
        assert_eq!(keypad.read(4, 2), 16 | 8 << 5);
        keypad.set_keys(0);
        for _ in 0..3 { assert!(keypad.irq_pending()); keypad.write(8, 7); }
        assert!(!keypad.irq_pending());
    }

    #[test]
    fn release_reports_remaining_matrix_and_zero_when_all_contacts_are_up() {
        let mut keypad = Keypad::default();
        keypad.write(0, 3);
        keypad.set_keys(PhoneKey::Down.bit() | PhoneKey::Ok.bit());
        keypad.write(8, 7);
        keypad.write(8, 7);
        keypad.set_keys(PhoneKey::Ok.bit());
        assert_eq!(keypad.read(8, 1), 5);
        assert_eq!(keypad.read(4, 2), 16 | 8 << 5);
        keypad.write(8, 7);
        keypad.set_keys(0);
        assert_eq!(keypad.read(8, 1), 5);
        assert_eq!(keypad.read(4, 2), 0);
        keypad.write(8, 7);
        assert!(!keypad.irq_pending());
    }

    #[test]
    fn reset_and_bounded_overflow() {
        let mut keypad = Keypad::default();
        for _ in 0..100 { keypad.set_keys(PhoneKey::One.bit()); keypad.set_keys(0); }
        assert_eq!(keypad.events.len(), 64);
        keypad.write(0, 4);
        assert!(keypad.irq_pending());
        keypad.write(8, 4);
        assert!(!keypad.irq_pending());
        keypad.write(0, 0x18);
        assert_eq!(keypad.read(8, 1), 7);
    }
}
