//! CPU setup for the original E830 board.
use super::{E830Machine, LCD_HEIGHT, LCD_WIDTH};
use crate::core::cpu::{CpuConfig, Mode, RegisterARM};
use crate::core::report::ExecutionReport;
use crate::devices::runtime::{
    DeviceInfo, DeviceProfile, DeviceRuntime, DisplayInfo, Frame, RunReport, RuntimeOptions,
};
use crate::firmware::{e830::FirmwareSet, LoadedFirmware};

pub(super) const CPU_CONFIG: CpuConfig = CpuConfig {
    // LITTLE_ENDIAN is the zero-valued default flag.
    mode: Mode::ARM946,
    model: None,
    initial_registers: &[
        (RegisterARM::FPEXC, 0x4000_0000),
        (RegisterARM::C1_C0_2, 0x00f0_0000),
    ],
};

pub struct E830Profile;

impl DeviceProfile for E830Profile {
    fn info(&self) -> DeviceInfo {
        DeviceInfo {
            id: "e830",
            name: "Samsung SGH-E830",
            default_loader: "e830-cla",
            display: Some(DisplayInfo {
                width: LCD_WIDTH,
                height: LCD_HEIGHT,
                format: "RGB565",
            }),
            support: "Existing E830 board; original firmware boot and optional DSP HLE",
            has_nand: true,
            has_keys: true,
            storage_hint: Some("32 MiB main-only or 1024+32 per page. TFS is not a NAND image."),
        }
    }

    fn create(
        &self,
        firmware: LoadedFirmware,
        options: RuntimeOptions,
    ) -> anyhow::Result<Box<dyn DeviceRuntime>> {
        anyhow::ensure!(
            !options.stop_on_frame,
            "E830 does not yet support --stop-on-frame; use an instruction budget"
        );
        anyhow::ensure!(
            firmware.metadata.format == "e830-cla-nsf",
            "E830 requires a CLA/NSF firmware loader"
        );
        Ok(Box::new(E830Machine::new(
            FirmwareSet::from_input(firmware.input)?,
            options.trace_mmio,
        )?))
    }

    fn runtime_notice(&self) -> crate::devices::runtime::RuntimeNotice {
        let mode = super::frontend::RuntimeMode::from_environment();
        crate::devices::runtime::RuntimeNotice {
            label: mode.label(),
            detail: mode.notice().into(),
            experimental: mode.sim,
        }
    }
}

impl DeviceRuntime for E830Machine {
    fn run(&mut self, budget: u64) -> anyhow::Result<RunReport> {
        let report = E830Machine::run(self, budget)?;
        Ok(RunReport {
            device: "e830".into(),
            execution: ExecutionReport {
                instructions: report.instructions,
                fast_forwarded_instructions: report.fast_forwarded_instructions,
                pc: report.pc,
                cpsr: report.cpsr,
                r0: report.r0,
                r1: report.r1,
                r2: report.r2,
                r3: report.r3,
                r4: report.r4,
                r5: report.r5,
                r6: report.r6,
                r7: report.r7,
                sp: report.sp,
                lr: report.lr,
                stop_reason: report.stop_reason,
                memory_fault: report.memory_fault,
                recent_mmio: report.recent_mmio,
            },
            lcd_frames: report.lcd_frames,
            details: serde_json::json!({ "irqs_taken": report.irqs_taken,
                "keyboard_init_dispatches": report.keyboard_init_dispatches,
                "standby_look_dispatches": report.standby_look_dispatches,
                "eeprom_size": report.eeprom_size, "tfs_size": report.tfs_size,
                "nor_size": report.nor_size, "onenand_image_size": report.onenand_image_size }),
        })
    }

    fn frame(&mut self) -> Option<Frame> {
        let revision = self.lcd_write_revision();
        self.lcd_snapshot_rgb().map(|rgb| Frame {
            width: LCD_WIDTH,
            height: LCD_HEIGHT,
            rgb,
            revision,
        })
    }
    fn frame_revision(&mut self) -> u64 {
        self.lcd_write_revision()
    }
    fn set_phone_keys(&mut self, keys: u32) -> anyhow::Result<()> {
        E830Machine::set_phone_keys(self, keys);
        Ok(())
    }
    fn set_breakpoint(&mut self, pc: u32, after: u64) {
        E830Machine::set_breakpoint(self, pc, after);
    }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}
