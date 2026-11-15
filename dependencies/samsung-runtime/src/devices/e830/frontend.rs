//! Compatibility notices for the existing opt-in E830 launch modes.
#[derive(Clone, Copy, Default)]
pub struct RuntimeMode {
    pub sim: bool,
    pub boot_hle: bool,
    pub rf_hle: bool,
    pub battery_hle: bool,
}

impl RuntimeMode {
    pub fn from_environment() -> Self {
        Self {
            sim: std::env::var("E830_SIM_PRESENT").as_deref() == Ok("1"),
            boot_hle: std::env::var_os("E830_DSP_BOOT_HLE").is_some(),
            rf_hle: std::env::var("E830_DSP_RF_HLE").as_deref() == Ok("empty"),
            battery_hle: std::env::var("E830_ANALOG_HLE").as_deref() == Ok("battery"),
        }
    }

    pub fn label(self) -> String {
        format!("Mode: {} | DSP boot HLE: {} | RF HLE: {} | Battery HLE: {}",
            if self.sim { "experimental SIM" } else { "no SIM" },
            if self.boot_hle { "on" } else { "off" },
            if self.rf_hle { "empty air" } else { "off" },
            if self.battery_hle { "3.7 V" } else { "off" })
    }

    pub fn notice(self) -> &'static str {
        if self.sim {
            "Experimental SIM boot is incomplete. Startup animation is not a verified SIM standby or app menu."
        } else if self.boot_hle {
            "No-SIM standby is verified with the installed TFS NAND. OK opens the app menu; arrows navigate it."
        } else {
            "DSP boot HLE is off. Use Run-E830-NoSIM.cmd for the verified no-card boot path."
        }
    }
}
