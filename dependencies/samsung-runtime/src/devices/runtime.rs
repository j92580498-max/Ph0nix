//! Shared runtime contract. Device implementations own all hardware behavior.
use crate::core::report::ExecutionReport;
use crate::firmware::LoadedFirmware;
use anyhow::{ensure, Result};
use serde::Serialize;
use std::any::Any;

#[derive(Clone, Copy, Debug, Serialize)]
pub struct DisplayInfo {
    pub width: usize,
    pub height: usize,
    pub format: &'static str,
}

#[derive(Clone, Copy, Debug, Serialize)]
pub struct DeviceInfo {
    pub id: &'static str,
    pub name: &'static str,
    pub default_loader: &'static str,
    pub display: Option<DisplayInfo>,
    pub support: &'static str,
    pub has_nand: bool,
    pub has_keys: bool,
    pub storage_hint: Option<&'static str>,
}

pub struct RuntimeNotice {
    pub label: String,
    pub detail: String,
    pub experimental: bool,
}

#[derive(Clone, Debug)]
pub struct Frame {
    pub width: usize,
    pub height: usize,
    pub rgb: Vec<u8>,
    pub revision: u64,
}

impl Frame {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.width != 0 && self.height != 0,
            "The framebuffer dimensions are empty"
        );
        ensure!(
            self.width
                .checked_mul(self.height)
                .and_then(|size| size.checked_mul(3))
                == Some(self.rgb.len()),
            "The RGB framebuffer does not match its dimensions"
        );
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct RuntimeOptions {
    pub trace_mmio: bool,
    pub stop_on_frame: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct RunReport {
    pub device: String,
    pub execution: ExecutionReport,
    pub lcd_frames: u64,
    pub details: serde_json::Value,
}

/// Rust plugins implement this contract; they can use any configured core CPU/bus.
pub trait DeviceRuntime {
    fn run(&mut self, budget: u64) -> Result<RunReport>;
    fn frame(&mut self) -> Option<Frame>;
    fn frame_revision(&mut self) -> u64;
    fn set_phone_keys(&mut self, keys: u32) -> Result<()>;
    fn set_breakpoint(&mut self, pc: u32, after: u64);
    fn as_any_mut(&mut self) -> &mut dyn Any;
}

pub trait DeviceProfile: Send + Sync {
    fn info(&self) -> DeviceInfo;
    fn create(
        &self,
        firmware: LoadedFirmware,
        options: RuntimeOptions,
    ) -> Result<Box<dyn DeviceRuntime>>;
    fn runtime_notice(&self) -> RuntimeNotice {
        RuntimeNotice {
            label: self.info().name.into(),
            detail: self.info().support.into(),
            experimental: true,
        }
    }
}
