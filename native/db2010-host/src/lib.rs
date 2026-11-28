// SPDX-License-Identifier: GPL-2.0-or-later
//! Native hosts share preparation, QMP, complete LCD frames and keypad contacts.
pub mod prepare;
mod flash;
pub mod qmp;
pub mod audio;
pub mod bluetooth;
pub mod diagnostics;
mod display;
pub mod storage;

use anyhow::{bail, Result};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct HostOptions {
    pub engine: PathBuf,
    pub midi_soundfont: Option<PathBuf>,
    pub data_dir: PathBuf,
    pub resume: bool,
}

impl HostOptions {
    pub fn app_data(root: &Path) -> PathBuf {
        if cfg!(windows) {
            std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
                .unwrap_or_else(|| root.to_owned()).join("FeaturePhoneEmulator")
        } else {
            std::env::var_os("XDG_DATA_HOME").map(PathBuf::from)
                .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
                .unwrap_or_else(|| root.to_owned()).join("featurephone-emulator")
        }
    }
    pub fn discover(root: &Path) -> Result<Self> {
        let suffix = if cfg!(windows) { ".exe" } else { "" };
        let engine = std::env::var_os("FEATUREPHONE_QEMU").map(PathBuf::from).or_else(|| {
            [root.join(format!("engine/qemu-system-arm{suffix}")),
             root.join(format!("qemu/build/qemu-system-arm{suffix}")),
             root.join(format!("qemu-system-arm{suffix}"))].into_iter().find(|path| path.is_file())
        });
        let Some(engine) = engine else {
            bail!("The native DB2010 engine is missing. Build QEMU or set FEATUREPHONE_QEMU to qemu-system-arm.");
        };
        let data_dir = Self::app_data(root);
        Ok(Self { engine, data_dir, resume: true, midi_soundfont: None })
    }
}

#[derive(Clone, Debug)]
pub struct LcdFrame {
    pub width: usize,
    pub height: usize,
    pub rgb565_be: Vec<u8>,
    pub revision: u64,
}

impl LcdFrame {
    pub fn rgb(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.width * self.height * 3);
        for pixel in self.rgb565_be.chunks_exact(2) {
            let word = u16::from_be_bytes([pixel[0],pixel[1]]) as u32;
            out.extend_from_slice(&[((word >> 11)*255/31) as u8,
                (((word >> 5)&63)*255/63) as u8, ((word&31)*255/31) as u8]);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_rgb565_preserves_byte_order_and_color_scaling() {
        let frame = LcdFrame { width:4, height:1, revision:1,
            rgb565_be: vec![0xf8,0, 0x07,0xe0, 0,0x1f, 0xf7,0x9e] };
        assert_eq!(frame.rgb(), [255,0,0, 0,255,0, 0,0,255, 246,242,246]);
    }
}
