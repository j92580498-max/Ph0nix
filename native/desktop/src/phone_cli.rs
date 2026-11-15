// Derived from tayga16/SamsungE380-Emu; original Apache-2.0/MIT terms retained.
//! Profile-neutral CLI; device file roles are supplied by firmware loaders.
use anyhow::{ensure, Context, Result};
use clap::{Args, Parser, Subcommand};
use e830::devices::runtime::RuntimeOptions;
use e830::firmware::FirmwareFiles;
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Parser)]
#[command(about = "Run phone firmware using an explicit device profile and image loader")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// List registered phones and their display/support information.
    Profiles,
    /// Inspect an image without creating a CPU.
    Inspect(Input),
    /// Execute from the firmware reset entry and export the guest LCD.
    Run {
        #[command(flatten)]
        input: Input,
        #[arg(long, default_value_t = 10_000_000)]
        instructions: u64,
        /// Resume execution in bounded chunks, as the GUI worker does.
        #[arg(long)]
        run_quantum: Option<u64>,
        #[arg(long)]
        lcd: Option<PathBuf>,
        #[arg(long)]
        report: Option<PathBuf>,
        #[arg(long)]
        trace_mmio: bool,
        /// Stop after a completed display transfer with visible pixels (N500).
        #[arg(long)]
        stop_on_frame: bool,
        /// Use the incomplete instruction-budget DB2010 research board.
        #[arg(long)]
        research_board: bool,
    },
}

#[derive(Args)]
struct Input {
    #[arg(long)]
    device: String,
    #[arg(long)]
    loader: Option<String>,
    #[arg(long)]
    image: PathBuf,
    /// Loader file role, e.g. eeprom=phone.nsf or ogm=phone.ogm (repeatable).
    #[arg(long, value_parser = parse_companion)]
    companion: Vec<(String, PathBuf)>,
}

fn parse_companion(text: &str) -> Result<(String, PathBuf), String> {
    let (role, path) = text
        .split_once('=')
        .ok_or("Use ROLE=PATH for a companion file")?;
    if role.is_empty() || path.is_empty() || role == "image" {
        return Err(
            "A companion needs a nonempty role and path; use --image for the primary image".into(),
        );
    }
    Ok((role.into(), path.into()))
}

impl Input {
    fn files(&self) -> Result<FirmwareFiles> {
        let mut companions = BTreeMap::new();
        for (role, path) in &self.companion {
            ensure!(
                companions.insert(role.clone(), path.clone()).is_none(),
                "Duplicate companion role {role}"
            );
        }
        Ok(FirmwareFiles {
            image: self.image.clone(),
            companions,
        })
    }
}

fn main() -> Result<()> {
    let registry = k750_native::registry();
    match Cli::parse().command {
        Command::Profiles => println!("{}", serde_json::to_string_pretty(&registry.devices())?),
        Command::Inspect(input) => {
            let firmware =
                registry.load(&input.device, input.loader.as_deref(), &input.files()?)?;
            println!("{}", serde_json::to_string_pretty(&firmware.metadata)?);
        }
        Command::Run {
            input,
            instructions,
            run_quantum,
            lcd,
            report,
            trace_mmio,
            stop_on_frame,
            research_board,
        } => {
            ensure!(input.device != "k750i" || research_board,
                "K750i apps use the native QEMU host. Run featurephone-emulator or db2010-host-check; --research-board explicitly selects the incomplete Unicorn investigation path.");
            ensure!(
                instructions > 0,
                "The instruction budget must be greater than zero"
            );
            let quantum = run_quantum.unwrap_or(instructions);
            ensure!(quantum > 0, "The run quantum must be greater than zero");
            let mut machine = registry.open(
                &input.device,
                input.loader.as_deref(),
                &input.files()?,
                RuntimeOptions {
                    trace_mmio,
                    stop_on_frame,
                },
            )?;
            let mut outcome = machine.run(instructions.min(quantum))?;
            while outcome.execution.instructions < instructions
                && matches!(
                    outcome.execution.stop_reason,
                    e830::StopReason::InstructionBudget
                )
            {
                let previous = outcome.execution.instructions;
                outcome = machine.run((instructions - previous).min(quantum))?;
                ensure!(
                    outcome.execution.instructions > previous
                        || !matches!(
                            outcome.execution.stop_reason,
                            e830::StopReason::InstructionBudget
                        ),
                    "Device runtime stopped making instruction progress"
                );
            }
            // Preserve the stop diagnostic even if the requested LCD is absent.
            let json = serde_json::to_string_pretty(&outcome)?;
            if let Some(path) = report {
                std::fs::write(&path, &json)
                    .with_context(|| format!("writing {}", path.display()))?;
            }
            println!("{json}");
            if let Some(path) = lcd {
                let frame = machine
                    .frame()
                    .context("Firmware has not written to the LCD")?;
                frame.validate()?;
                let mut ppm = format!("P6\n{} {}\n255\n", frame.width, frame.height).into_bytes();
                ppm.extend_from_slice(&frame.rgb);
                std::fs::write(&path, ppm)
                    .with_context(|| format!("writing {}", path.display()))?;
            }
            if let e830::StopReason::CpuError(error) = &outcome.execution.stop_reason {
                anyhow::bail!(
                    "{} stopped at 0x{:08x}: {error}",
                    input.device,
                    outcome.execution.pc
                );
            }
        }
    }
    Ok(())
}
