//! Page-addressed monochrome LCD bus observed in N500 lcd_ctrl.o.
//! The controller command set matches ST7565-class parts; its exact part is unknown.
use crate::devices::runtime::Frame;

pub const WIDTH: usize = 128;
pub const HEIGHT: usize = 64;
const COLUMNS: usize = 132;
const PAGES: usize = HEIGHT / 8;
const COLUMN_OFFSET: usize = 4;

pub struct Lcd {
    ram: [u8; COLUMNS * PAGES],
    column: usize,
    page: usize,
    start_line: usize,
    contrast_parameter: bool,
    enabled: bool,
    inverse: bool,
    all_on: bool,
    written: [bool; WIDTH * PAGES],
    covered: usize,
    pub commands: u64,
    pub writes: u64,
    pub transfers: u64,
    pub visible_transfers: u64,
}

impl Default for Lcd {
    fn default() -> Self {
        Self {
            ram: [0; COLUMNS * PAGES],
            column: 0,
            page: 0,
            start_line: 0,
            contrast_parameter: false,
            enabled: false,
            inverse: false,
            all_on: false,
            written: [false; WIDTH * PAGES],
            covered: 0,
            commands: 0,
            writes: 0,
            transfers: 0,
            visible_transfers: 0,
        }
    }
}

impl Lcd {
    pub fn command(&mut self, value: u8) -> Result<(), String> {
        self.commands += 1;
        if self.contrast_parameter {
            self.contrast_parameter = false;
            return Ok(());
        }
        match value {
            0x00..=0x0f => self.column = (self.column & 0xf0) | value as usize,
            0x10..=0x1f => self.column = (self.column & 0x0f) | ((value as usize & 0xf) << 4),
            0x40..=0x7f => self.start_line = value as usize & 0x3f,
            0x81 => self.contrast_parameter = true,
            0xae => self.enabled = false,
            0xaf => self.enabled = true,
            0xa4 => self.all_on = false,
            0xa5 => self.all_on = true,
            0xa6 => self.inverse = false,
            0xa7 => self.inverse = true,
            0xb0..=0xb8 => self.page = value as usize & 0xf,
            // Bias, supply and scan direction are programmed by lcd_ctrl.o.
            // The guest already uses column+4 for its 128 visible columns.
            0xa0 | 0xa1 | 0xa2 | 0xa3 | 0xc0 | 0xc8 | 0xe3 | 0x20..=0x2f => {}
            0xe2 => {
                self.column = 0;
                self.page = 0;
                self.start_line = 0;
                self.enabled = false;
                self.inverse = false;
                self.all_on = false;
                self.contrast_parameter = false;
            }
            _ => return Err(format!("Unimplemented N500 LCD command 0x{value:02x}")),
        }
        Ok(())
    }

    pub fn data(&mut self, value: u8) {
        self.writes += 1;
        if self.page < PAGES && self.column < COLUMNS {
            self.ram[self.page * COLUMNS + self.column] = value;
            if (COLUMN_OFFSET..COLUMN_OFFSET + WIDTH).contains(&self.column) {
                let index = self.page * WIDTH + self.column - COLUMN_OFFSET;
                if !self.written[index] {
                    self.written[index] = true;
                    self.covered += 1;
                }
            }
        }
        self.column = (self.column + 1) % COLUMNS;
        if self.covered == WIDTH * PAGES {
            self.transfers += 1;
            if self.enabled && self.has_pixels() {
                self.visible_transfers += 1;
            }
            self.covered = 0;
            self.written.fill(false);
        }
    }

    pub fn has_pixels(&self) -> bool {
        self.ram.chunks_exact(COLUMNS).any(|page| {
            page[COLUMN_OFFSET..COLUMN_OFFSET + WIDTH]
                .iter()
                .any(|value| *value != 0)
        })
    }

    pub fn frame(&self) -> Option<Frame> {
        if self.writes == 0 || !self.enabled {
            return None;
        }
        let mut rgb = Vec::with_capacity(WIDTH * HEIGHT * 3);
        for y in 0..HEIGHT {
            let row = (y + self.start_line) % HEIGHT;
            for x in 0..WIDTH {
                let set = self.all_on
                    || ((self.ram[(row / 8) * COLUMNS + x + COLUMN_OFFSET] & (1 << (row % 8))
                        != 0)
                        ^ self.inverse);
                rgb.extend_from_slice(if set { &[0, 0, 0] } else { &[255, 255, 255] });
            }
        }
        Some(Frame {
            width: WIDTH,
            height: HEIGHT,
            rgb,
            revision: self.writes + self.commands,
        })
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn address(lcd: &mut Lcd, page: u8, column: u8) {
        lcd.command(0xb0 + page).unwrap();
        lcd.command(0x10 | (column >> 4)).unwrap();
        lcd.command(column & 15).unwrap();
    }
    #[test]
    fn blank_lcd_is_not_a_guest_frame_and_partial_updates_preserve_pixels() {
        let mut lcd = Lcd::default();
        assert!(lcd.frame().is_none());
        address(&mut lcd, 1, 4);
        lcd.data(0x02);
        assert!(lcd.frame().is_none());
        lcd.command(0xaf).unwrap();
        let frame = lcd.frame().unwrap();
        frame.validate().unwrap();
        assert_eq!(&frame.rgb[(9 * WIDTH) * 3..(9 * WIDTH) * 3 + 3], &[0, 0, 0]);
        address(&mut lcd, 0, 5);
        lcd.data(1);
        assert_eq!(
            &lcd.frame().unwrap().rgb[(9 * WIDTH) * 3..(9 * WIDTH) * 3 + 3],
            &[0, 0, 0]
        );
        lcd.command(0x81).unwrap();
        lcd.command(0x04).unwrap();
        lcd.command(0xa7).unwrap();
        assert_eq!(
            &lcd.frame().unwrap().rgb[(9 * WIDTH) * 3..(9 * WIDTH) * 3 + 3],
            &[255, 255, 255]
        );
    }
    #[test]
    fn visible_transfers_require_coverage_and_guest_pixels() {
        let mut lcd = Lcd::default();
        lcd.command(0xaf).unwrap();
        for page in 0..8 {
            address(&mut lcd, page, 4);
            for _ in 0..128 {
                lcd.data(0);
            }
        }
        assert_eq!((lcd.transfers, lcd.visible_transfers), (1, 0));
        for page in 0..8 {
            address(&mut lcd, page, 4);
            for _ in 0..128 {
                lcd.data(1);
            }
        }
        assert_eq!((lcd.transfers, lcd.visible_transfers), (2, 1));
    }
}
