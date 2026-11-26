// SPDX-License-Identifier: GPL-2.0-or-later
//! Check native file transfer across save/resume or a saved-storage cold boot.
use anyhow::{ensure, Context, Result};
use db2010_host::{
    prepare,
    qmp::Session,
    storage::{FileJob, FileRequest},
    HostOptions,
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

fn wait_for_serial(session: &mut Session, workspace: &Path) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(300);
    loop {
        let progress = session.progress()?;
        if progress["running"].as_bool() != Some(true) {
            let snapshot = (|| -> Result<()> {
                fs::write(
                    workspace.join("failure-snapshot-chunks.json"),
                    serde_json::to_vec(&session.snapshot_chunks()?)?,
                )?;
                Ok(())
            })();
            if let Err(error) = snapshot {
                eprintln!("Could not capture the boot failure snapshot: {error:#}");
            }
            anyhow::bail!("Firmware stopped during cold boot: {}", session.summary()?);
        }
        if progress["serial_input_ready"].as_bool() == Some(true)
            && progress["serial_cable_present"].as_bool() == Some(true)
        {
            return Ok(());
        }
        ensure!(
            Instant::now() < deadline,
            "Phone file transport did not become ready after cold boot: {progress}"
        );
        thread::sleep(Duration::from_millis(200));
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
    let directory = vec!["Phone memory".to_owned(), "Other".to_owned()];
    let mut path = directory.clone();
    path.push("ph0nix-resume-test.txt".into());
    {
        println!("{}", serde_json::json!({"phase":"initial_boot","state":state}));
        let mut session = Session::open(&engine, &state, checkpoint.as_deref())?;
        if checkpoint.is_none() {
            wait_for_serial(&mut session, &workspace)?;
        }
        run(
            &mut session,
            FileRequest::List {
                path: directory.clone(),
            },
        )?;
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
            wait_for_serial(&mut session, &workspace)?;
        }
        run(
            &mut session,
            FileRequest::List {
                path: directory.clone(),
            },
        )?;
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
