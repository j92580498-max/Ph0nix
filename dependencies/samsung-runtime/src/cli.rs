use std::io::Write;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;
use e830::{E830Machine, FirmwareSet, PhoneKey, StopReason, LCD_HEIGHT, LCD_WIDTH};

use e830::firmware::e830::provision;

#[derive(Parser)]
#[command(about = "Samsung SGH-E830 firmware boot probe", version)]
struct Args {
    /// Original E830 .cla firmware
    cla: PathBuf,
    /// Matching .nsf EEPROM image
    eeprom: PathBuf,
    /// Retail TFS installer package (physical filesystem installation is not implemented)
    #[arg(long)]
    tfs: Option<PathBuf>,
    /// Optional diagnostic loader source at 0x1c000000 (takes precedence over --tfs)
    #[arg(long)]
    tfs_source: Option<PathBuf>,
    /// Physical OneNAND dump: 32 MiB main-only or 1024+32 bytes per page
    #[arg(long)]
    onenand_image: Option<PathBuf>,
    /// Save current physical OneNAND including guest writes; destination must be new
    #[arg(long)]
    dump_onenand: Option<PathBuf>,
    /// Diagnostic original-firmware call script; requires a reached --break-at
    #[arg(long, requires = "break_at")]
    firmware_script: Option<PathBuf>,
    #[arg(long, default_value_t = 1_200_000_000)]
    max_instructions: u64,
    /// Maximum instructions per Unicorn run, for reproducing GUI worker resumes
    #[arg(long, value_parser = clap::value_parser!(u64).range(1..))]
    run_quantum: Option<u64>,
    /// Write a new JSON host/guest profile; never overwrites an existing file
    #[arg(long)]
    profile: Option<PathBuf>,
    #[arg(long, default_value_t = 4096, value_parser = clap::value_parser!(u64).range(1..))]
    profile_interval: u64,
    #[arg(long, default_value_t = 0)]
    profile_after: u64,
    #[arg(long, default_value_t = u64::MAX)]
    profile_before: u64,
    #[arg(long)]
    dump_lcd_frame: Option<PathBuf>,
    #[arg(long)]
    trace_mmio: bool,
    #[arg(long, value_parser = parse_address)]
    peek: Vec<u32>,
    /// Stop before executing this PC (ARM or Thumb)
    #[arg(long, value_parser = parse_address)]
    break_at: Option<u32>,
    /// Start looking for the breakpoint after this instruction count
    #[arg(long, default_value_t = 0)]
    break_after: u64,
    /// Save the guest's 32 MiB SDRAM for analysis
    #[arg(long)]
    dump_ram: Option<PathBuf>,
    /// Save bytes transmitted by the first UART (boot/download serial port)
    #[arg(long)]
    dump_uart: Option<PathBuf>,
    /// Save DSP boot memory space 0 as little-endian 16-bit words
    #[arg(long)]
    dump_dsp_space0: Option<PathBuf>,
    /// Save DSP boot memory space 1 as little-endian 16-bit words
    #[arg(long)]
    dump_dsp_space1: Option<PathBuf>,
    /// External phone contact transition: INSTRUCTION:KEY:down|up (repeatable)
    #[arg(long, value_parser = parse_key_transition)]
    key_at: Vec<KeyTransition>,
    /// Read-only LCD checkpoint: INSTRUCTION:OUTPUT.ppm (repeatable)
    #[arg(long, value_parser = parse_lcd_checkpoint)]
    capture_at: Vec<LcdCheckpoint>,
}

#[derive(Clone, Debug)]
struct KeyTransition { at: u64, key: PhoneKey, down: bool }

#[derive(Clone, Debug)]
struct LcdCheckpoint { at: u64, path: PathBuf }

fn parse_lcd_checkpoint(raw: &str) -> Result<LcdCheckpoint, String> {
    let (at, path) = raw.split_once(':').ok_or("Expected INSTRUCTION:OUTPUT.ppm")?;
    if path.is_empty() { return Err("LCD checkpoint requires an output path".into()); }
    Ok(LcdCheckpoint { at: at.parse::<u64>().map_err(|error| error.to_string())?, path: path.into() })
}

fn save_lcd_checkpoint(machine: &mut E830Machine, path: &std::path::Path) -> Result<()> {
    let rgb = machine.lcd_snapshot_rgb().context("LCD checkpoint has no framebuffer")?;
    let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(path)
        .context("creating a new LCD checkpoint")?;
    write!(file, "P6\n{LCD_WIDTH} {LCD_HEIGHT}\n255\n")?;
    file.write_all(&rgb)?;
    Ok(())
}

fn run_interval(machine: &mut E830Machine, instructions: u64, quantum: Option<u64>) -> Result<e830::BootReport> {
    let mut remaining = instructions;
    loop {
        let step = remaining.min(quantum.unwrap_or(remaining));
        let report = machine.run(step)?;
        if report.memory_fault.is_some() || report.stop_reason != StopReason::InstructionBudget || step == remaining {
            return Ok(report);
        }
        remaining -= step;
    }
}

fn parse_key_transition(raw: &str) -> Result<KeyTransition, String> {
    let parts: Vec<_> = raw.split(':').collect();
    if parts.len() != 3 { return Err("Expected INSTRUCTION:KEY:down|up".into()); }
    Ok(KeyTransition {
        at: parts[0].parse::<u64>().map_err(|error| error.to_string())?,
        key: PhoneKey::parse(parts[1]).ok_or_else(|| "Unknown phone key".to_string())?,
        down: match parts[2] { "down" => true, "up" => false, _ => return Err("Use down or up".into()) },
    })
}

fn parse_address(raw: &str) -> Result<u32, String> {
    raw.strip_prefix("0x").or_else(|| raw.strip_prefix("0X"))
        .map(|value| u32::from_str_radix(value, 16))
        .unwrap_or_else(|| raw.parse()).map_err(|error| error.to_string())
}

fn main() -> Result<()> {
    let args = Args::parse();
    let mut firmware = FirmwareSet::load(&args.cla, &args.eeprom, args.tfs_source.as_deref(), args.tfs.as_deref())?;
    if let Some(path) = args.onenand_image.as_deref() {
        firmware = firmware.with_onenand_image(path)?;
    }
    let mut machine = E830Machine::new(firmware, args.trace_mmio)?;
    if let Some(path) = &args.profile {
        anyhow::ensure!(!path.exists(), "profile output already exists: {}", path.display());
        anyhow::ensure!(args.profile_before > args.profile_after, "profile-before must be greater than profile-after");
        machine.enable_profiler(e830::ProfileConfig {
            sample_interval: args.profile_interval, after_instruction: args.profile_after,
            before_instruction: args.profile_before,
        });
    }
    if let Some(pc) = args.break_at { machine.set_breakpoint(pc, args.break_after); }
    let mut timeline: Vec<_> = args.key_at.into_iter()
        .map(|event| (event.at, Some(event), None)).collect();
    timeline.extend(args.capture_at.into_iter().map(|event| (event.at, None, Some(event.path))));
    timeline.sort_by_key(|event| event.0);
    anyhow::ensure!(timeline.iter().all(|event| event.0 < args.max_instructions),
        "input and LCD checkpoints must precede max-instructions");
    let mut keys = 0;
    let mut executed = 0;
    let mut stopped = None;
    for (at, key_event, capture_path) in timeline {
        if at > executed {
            let report = run_interval(&mut machine, at - executed, args.run_quantum)?;
            executed = report.instructions;
            if report.memory_fault.is_some() || report.stop_reason != StopReason::InstructionBudget {
                stopped = Some(report); break;
            }
        }
        if let Some(event) = key_event {
            if event.down { keys |= event.key.bit(); } else { keys &= !event.key.bit(); }
            machine.set_phone_keys(keys);
            println!("Phone contact at {executed}: {:?} {}", event.key, if event.down { "down" } else { "up" });
        }
        if let Some(path) = capture_path {
            save_lcd_checkpoint(&mut machine, &path)?;
            println!("LCD checkpoint at {executed}: {}", path.display());
        }
    }
    let report = match stopped {
        Some(report) => report,
        None => run_interval(&mut machine, args.max_instructions - executed, args.run_quantum)?,
    };
    println!("{report:#?}");
    if let Some(path) = args.firmware_script {
        anyhow::ensure!(report.stop_reason == StopReason::Breakpoint && report.memory_fault.is_none(),
            "provisioning requires a reached breakpoint without a memory fault");
        provision::execute(&mut machine, &path)?;
    }
    if let Some(path) = args.dump_onenand {
        let mut output = std::fs::OpenOptions::new().write(true).create_new(true).open(&path)
            .context("creating a new NAND capture (existing files are preserved)")?;
        output.write_all(&machine.onenand_snapshot()).context("saving physical NAND capture")?;
        println!("OneNAND saved: {}", path.display());
    }
    if let Some(path) = args.dump_uart {
        std::fs::write(&path, machine.uart_output(0).unwrap_or_default())
            .context("saving UART diagnostic output")?;
        println!("UART saved: {}", path.display());
    }
    if let Some(path) = args.dump_ram {
        std::fs::write(&path, machine.sdram_snapshot()?).context("saving diagnostic SDRAM capture")?;
        println!("SDRAM saved: {}", path.display());
    }
    for (space, path) in [(0, args.dump_dsp_space0), (1, args.dump_dsp_space1)] {
        if let Some(path) = path {
            let bytes = machine.dsp_memory_snapshot(space).context("reading DSP boot memory")?;
            std::fs::write(&path, bytes).context("saving DSP boot memory")?;
            println!("DSP space {space} saved: {}", path.display());
        }
    }
    for address in args.peek {
        println!("peek 0x{address:08x}: {:?}", machine.peek_u32(address));
    }
    if let Some(path) = args.dump_lcd_frame {
        if let Some(rgb) = machine.lcd_snapshot_rgb() {
            let mut file = std::fs::File::create(&path).context("creating LCD capture")?;
            write!(file, "P6\n{LCD_WIDTH} {LCD_HEIGHT}\n255\n")?;
            file.write_all(&rgb)?;
            println!("LCD saved: {}", path.display());
        } else {
            println!("No complete LCD frame was produced.");
        }
    }
    if let Some(path) = args.profile {
        machine.save_profile(&path).context("saving new profiler report")?;
        println!("Profile saved: {}", path.display());
    }
    if let Some(fault) = report.memory_fault { anyhow::bail!("Guest memory fault: {fault:?}"); }
    if let StopReason::CpuError(error) = report.stop_reason { anyhow::bail!("CPU stopped: {error}"); }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phone_transition_parser_rejects_ambiguous_or_invalid_input() {
        let event = parse_key_transition("600000000:soft-left:down").unwrap();
        assert_eq!(event.at, 600_000_000);
        assert_eq!(event.key, PhoneKey::SoftLeft);
        assert!(event.down);
        assert!(!parse_key_transition("610000000:ok:up").unwrap().down);
        for raw in ["ok", "1:unknown:down", "-1:ok:up", "1:ok:click", "1:ok:up:extra"] {
            assert!(parse_key_transition(raw).is_err());
        }
    }

    #[test]
    fn lcd_checkpoint_parser_preserves_windows_drive_colon() {
        let checkpoint = parse_lcd_checkpoint("900000000:C:\\proof\\menu.ppm").unwrap();
        assert_eq!(checkpoint.at, 900_000_000);
        assert_eq!(checkpoint.path, PathBuf::from("C:\\proof\\menu.ppm"));
        for raw in ["menu.ppm", "-1:menu.ppm", "900:"] {
            assert!(parse_lcd_checkpoint(raw).is_err());
        }
    }
}
