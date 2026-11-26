// SPDX-License-Identifier: GPL-2.0-or-later
//! Check native file transfer across save/resume or a saved-storage cold boot.
use anyhow::{ensure, Context, Result};
use db2010_host::{
    prepare,
    qmp::Session,
    storage::{FileEntry, FileJob, FileRequest},
    HostOptions, LcdFrame,
};
use std::{
    fs,
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

fn run(session: &mut Session, request: FileRequest) -> Result<FileJob> {
    let deadline = Instant::now() + Duration::from_secs(45);
    while !session.files.snapshot().attached {
        ensure!(Instant::now() < deadline, "File transport did not attach");
        thread::sleep(Duration::from_millis(20));
    }
    let id = session.files.request(request)?;
    loop {
        let snapshot = session.files.snapshot();
        if let Some(job) = snapshot.job {
            if job.id == id && !snapshot.busy {
                println!("{}", serde_json::to_string(&job)?);
                ensure!(
                    job.phase == "complete",
                    "File operation failed: {}",
                    job.error.unwrap_or_default()
                );
                return Ok(job);
            }
        }
        ensure!(Instant::now() < deadline, "File job deadline exceeded");
        ensure!(
            session.progress()?["running"].as_bool() == Some(true),
            "Firmware stopped during file operation"
        );
        thread::sleep(Duration::from_millis(50));
    }
}

fn capture_panel(session: &mut Session, name: &str) -> Result<()> {
    let Some(directory) = std::env::var_os("PH0NIX_TEST_LCD_DIR") else {
        return Ok(());
    };
    // Read the cached panel even when the firmware has not redrawn it.
    let panel = session.qmp.property("/machine", "display-cached")?;
    if let Some(hex) = panel["pixels_hex"]
        .as_str()
        .filter(|hex| hex.len() == 176 * 220 * 4 && hex.is_ascii())
    {
        let rgb565_be = (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16))
            .collect::<Result<Vec<_>, _>>()?;
        let frame = LcdFrame {
            width: 176,
            height: 220,
            rgb565_be,
            revision: 0,
        };
        let directory = PathBuf::from(directory);
        fs::create_dir_all(&directory)?;
        let mut ppm = format!("P6\n{} {}\n255\n", frame.width, frame.height).into_bytes();
        ppm.extend(frame.rgb());
        fs::write(directory.join(format!("{name}.ppm")), ppm)?;
    }
    Ok(())
}

fn report_boot_failure(session: &mut Session, workspace: &Path, phase: &str) {
    if let Err(error) = capture_panel(session, &format!("{phase}-failure")) {
        eprintln!("Could not capture the LCD at failure: {error:#}");
    }
    let snapshot = (|| -> Result<()> {
        let progress = session.progress()?;
        eprintln!(
            "{phase} failure progress: {}",
            serde_json::json!({
                "running": progress["running"], "pc": progress["pc"],
                "virtual_ns": progress["virtual_ns"], "serial_input_ready": progress["serial_input_ready"],
                "serial_cable_present": progress["serial_cable_present"]
            })
        );
        if progress["running"].as_bool() == Some(true) {
            session.qmp.execute("stop", serde_json::json!({}))?;
        }
        fs::write(
            workspace.join("failure-snapshot-chunks.json"),
            serde_json::to_vec(&session.snapshot_chunks()?)?,
        )?;
        Ok(())
    })();
    if let Err(error) = snapshot {
        eprintln!("Could not capture the boot failure snapshot: {error:#}");
    }
}

fn wait_for_serial(
    session: &mut Session,
    workspace: &Path,
    phase: &str,
    min_seconds: u64,
) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(min_seconds * 3 + 30);
    let mut frames = [40, 90, 150, min_seconds]
        .into_iter()
        .filter(|&t| t <= min_seconds)
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .peekable();
    loop {
        let progress = session.progress()?;
        if progress["running"].as_bool() != Some(true) {
            report_boot_failure(session, workspace, phase);
            anyhow::bail!("Firmware stopped during {phase}: {}", session.summary()?);
        }
        let seconds = progress["virtual_ns"].as_u64().context("Guest clock missing")? / 1_000_000_000;
        while let Some(&capture_at) = frames.peek() {
            if seconds < capture_at { break; }
            capture_panel(session, &format!("{phase}-{capture_at}s"))?;
            frames.next();
        }
        if seconds >= min_seconds && progress["serial_input_ready"].as_bool() == Some(true)
            && progress["serial_cable_present"].as_bool() == Some(true)
        {
            return Ok(());
        }
        if Instant::now() >= deadline {
            report_boot_failure(session, workspace, phase);
            anyhow::bail!("Phone file transport did not become ready during {phase}");
        }
        thread::sleep(Duration::from_millis(200));
    }
}

fn finish_first_setup(session: &mut Session) -> Result<()> {
    // The fresh R1DB001 test on CT100 reaches the language-selection page.
    // Select the highlighted language, decline the wizard, acknowledge its message.
    for (step, key) in [
        ("language", "soft-left"),
        ("skip-wizard", "soft-right"),
        ("acknowledge", "soft-left"),
    ] {
        session.contacts(&[key])?;
        thread::sleep(Duration::from_millis(150));
        session.contacts(&[])?;
        thread::sleep(Duration::from_secs(3));
        capture_panel(session, &format!("initial-boot-after-{step}"))?;
    }
    Ok(())
}

fn list(session: &mut Session, path: Vec<String>) -> Result<Vec<FileEntry>> {
    run(session, FileRequest::List { path: path.clone() })?;
    let entries = session.files.snapshot().entries;
    println!("{}", serde_json::json!({"path":path,"entries":entries}));
    Ok(entries)
}

fn find_folder<'a>(entries: &'a [FileEntry], names: &[&str]) -> Option<&'a str> {
    entries.iter().find(|entry| {
        entry.directory && names.iter().any(|name| entry.name.to_lowercase() == name.to_lowercase())
    }).map(|entry| entry.name.as_str())
}

fn internal_other(session: &mut Session) -> Result<Vec<String>> {
    let root = list(session, vec![])?;
    let other_names = ["Other", "Прочее", "Другое"];
    if let Some(memory) = find_folder(&root, &["Phone memory", "Память телефона"]) {
        let entries = list(session, vec![memory.to_owned()])?;
        let other = find_folder(&entries, &other_names).context("No Other directory in phone memory")?;
        Ok(vec![memory.to_owned(), other.to_owned()])
    } else {
        // With no Memory Stick controller in this test, the PC Suite root can
        // expose the internal user directories directly.
        let other = find_folder(&root, &other_names).context("No internal Other directory in OBEX root")?;
        Ok(vec![other.to_owned()])
    }
}

fn main() -> Result<()> {
    let mut args = std::env::args_os().skip(1);
    let engine = PathBuf::from(args.next().context(
        "Usage: phone-files-resume ENGINE COPIED_STATE CHECKPOINT [cold-boot] | ENGINE --fresh DATA_DIR MAIN FS",
    )?);
    let input = args.next().context("Copied prepared state or --fresh required")?;
    let (state, checkpoint, cold_boot) = if input == "--fresh" {
        let data = PathBuf::from(args.next().context("New data directory required")?);
        let main = PathBuf::from(args.next().context("K750i MAIN required")?);
        let fs_image = PathBuf::from(args.next().context("K750i FS required")?);
        ensure!(!data.exists(), "Fresh test requires a new data directory");
        (prepare::prepare(&main, &fs_image, &data)?, None, true)
    } else {
        let checkpoint = PathBuf::from(args.next().context("Starting checkpoint required")?);
        let cold_boot = match args.next() {
            None => false,
            Some(mode) => {
                ensure!(mode == "cold-boot", "Unknown boot mode");
                true
            }
        };
        (PathBuf::from(input), Some(checkpoint), cold_boot)
    };
    ensure!(args.next().is_none(), "Unexpected arguments");
    let workspace = state
        .parent()
        .context("Prepared workspace required")?
        .to_owned();
    let source = workspace.join("resume-source.txt");
    fs::write(
        &source,
        b"ph0nix file transfer remains usable after save and resume\n",
    )?;
    let directory;
    let path;
    {
        println!("{}", serde_json::json!({"phase":"initial_boot","state":state}));
        let mut session = Session::open(&engine, &state, checkpoint.as_deref())?;
        if checkpoint.is_none() {
            wait_for_serial(&mut session, &workspace, "initial-boot", 240)?;
            finish_first_setup(&mut session)?;
        }
        directory = match internal_other(&mut session) {
            Ok(directory) => directory,
            Err(error) => {
                report_boot_failure(&mut session, &workspace, "initial-boot");
                return Err(error);
            }
        };
        path = [directory.clone(), vec!["ph0nix-resume-test.txt".into()]].concat();
        run(
            &mut session,
            FileRequest::Import {
                path: path.clone(),
                host: source.clone(),
                replace: false,
                move_source: false,
            },
        )?;
        println!(
            "{}",
            serde_json::json!({"phase":"before_save","files":session.files.snapshot()})
        );
        // Deliberately leave the file service connected; Session owns safe quiescence.
        session.save()?;
    }
    {
        let options = HostOptions {
            engine,
            data_dir: workspace.clone(),
            resume: !cold_boot,
        };
        let mut session = Session::start(&options, &state)?;
        if cold_boot {
            wait_for_serial(&mut session, &workspace, "cold-boot", 150)?;
        }
        if let Err(error) = run(
            &mut session,
            FileRequest::List {
                path: directory.clone(),
            },
        ) {
            report_boot_failure(&mut session, &workspace, "cold-boot");
            return Err(error);
        }
        ensure!(
            session
                .files
                .snapshot()
                .entries
                .iter()
                .any(|entry| entry.name == "ph0nix-resume-test.txt"),
            "Imported file did not survive restarting the phone"
        );
        let output = workspace.join("resume-received.txt");
        run(
            &mut session,
            FileRequest::Export {
                path: path.clone(),
                host: output.clone(),
                move_source: false,
            },
        )?;
        ensure!(
            fs::read(output)? == fs::read(source)?,
            "Export contents after restart differ"
        );
        run(&mut session, FileRequest::Delete { path })?;
        println!(
            "{}",
            serde_json::json!({"phase":"after_restart","files":session.files.snapshot()})
        );
        session.save()?;
    }
    if cold_boot {
        println!("{}", serde_json::json!({"save_cold_boot_roundtrip":"passed"}));
    } else {
        println!("{}", serde_json::json!({"save_resume_roundtrip":"passed"}));
    }
    Ok(())
}
