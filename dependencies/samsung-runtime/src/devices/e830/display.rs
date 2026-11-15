//! Original E830 RGB565 LCD bus, windows, cursors and partial frames.
use super::*;

pub const LCD_WIDTH: usize = 176;
pub const LCD_HEIGHT: usize = 220;
pub(super) const LCD_PIXEL_COUNT: usize = LCD_WIDTH * LCD_HEIGHT;
pub(super) const LCD_GRAM_REGISTER: u16 = 0x22;

impl E830State {
    pub(super) fn write_lcd_register(&mut self, address: u32, size: usize, value: u64) {
        if size != 2 {
            return;
        }
        let address = u64::from(address);
        if address == SOC_EXTERNAL_CONTROL_BASE {
            self.lcd_bus_mode = value as u16;
            return;
        }
        if address != SOC_EXTERNAL_CONTROL_BASE + 2 {
            return;
        }
        let value = value as u16;
        if self.lcd_bus_mode == 0 {
            self.lcd_register_index = value;
        } else if self.lcd_bus_mode == 1 && self.lcd_register_index != LCD_GRAM_REGISTER {
            if let Some(register) = self.lcd_registers.get_mut(self.lcd_register_index as usize) {
                *register = value;
            }
            if self.lcd_register_index == 0x21 {
                // Original window routine 2119f1d0 writes x | (y << 8).
                let x = usize::from(value & 0xff);
                let y = usize::from(value >> 8);
                self.lcd_pixel_cursor = if x < LCD_WIDTH && y < LCD_HEIGHT {
                    y * LCD_WIDTH + x
                } else {
                    LCD_PIXEL_COUNT
                };
            }
        } else if self.lcd_bus_mode == 1 && self.lcd_register_index == LCD_GRAM_REGISTER {
            // Original 2119f1d0 packs inclusive x/y bounds into 44/45.
            // Initialization uses 44=af00, 45=db00 and entry mode 3=1030.
            // Cover that x-increasing, then y-increasing mode; other scan
            // directions are not inferred from this firmware path.
            let x0 = usize::from(self.lcd_registers[0x44] & 0xff);
            let x1 = usize::from(self.lcd_registers[0x44] >> 8);
            let y0 = usize::from(self.lcd_registers[0x45] & 0xff);
            let y1 = usize::from(self.lcd_registers[0x45] >> 8);
            let x = self.lcd_pixel_cursor % LCD_WIDTH;
            let y = self.lcd_pixel_cursor / LCD_WIDTH;
            if self.lcd_registers[3] & 0x38 != 0x30
                || x0 > x1 || x1 >= LCD_WIDTH || y0 > y1 || y1 >= LCD_HEIGHT
                || x < x0 || x > x1 || y < y0 || y > y1
            {
                return;
            }
            self.lcd_framebuffer[self.lcd_pixel_cursor] = value;
            self.lcd_pixel_cursor = if x < x1 {
                self.lcd_pixel_cursor + 1
            } else {
                (if y < y1 { y + 1 } else { y0 }) * LCD_WIDTH + x0
            };
            let next_x = self.lcd_pixel_cursor % LCD_WIDTH;
            let next_y = self.lcd_pixel_cursor / LCD_WIDTH;
            self.lcd_registers[0x21] = (next_x | (next_y << 8)) as u16;
            // Retain the report's full-screen-equivalent write counter. Partial
            // updates do not imply that every pixel was refreshed this cycle.
            self.lcd_pixels_since_frame += 1;
            if self.lcd_pixels_since_frame == LCD_PIXEL_COUNT {
                self.lcd_frames = self.lcd_frames.saturating_add(1);
                self.lcd_pixels_since_frame = 0;
            }
        }
    }

}

impl E830Machine {
    pub fn lcd_snapshot_rgb(&mut self) -> Option<Vec<u8>> {
        let state = self.cpu.raw_engine().get_data();
        if state.lcd_frames == 0 {
            return None;
        }
        let mut rgb = Vec::with_capacity(LCD_PIXEL_COUNT * 3);
        for &pixel in &state.lcd_framebuffer {
            rgb.push((((pixel >> 11) & 0x1f) * 255 / 31) as u8);
            rgb.push((((pixel >> 5) & 0x3f) * 255 / 63) as u8);
            rgb.push(((pixel & 0x1f) * 255 / 31) as u8);
        }
        Some(rgb)
    }

    /// Pixel-write revision includes partial UI updates, not just whole screens.
    pub fn lcd_write_revision(&mut self) -> u64 {
        let state = self.cpu.raw_engine().get_data();
        state.lcd_frames.saturating_mul(LCD_PIXEL_COUNT as u64)
            .saturating_add(state.lcd_pixels_since_frame as u64)
    }

}
