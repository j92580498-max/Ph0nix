// SPDX-License-Identifier: GPL-2.0-or-later
//! Check native file transfer across the save/resume lifecycle used by SAF.
use anyhow::{ensure, Context, Result};
use db2010_host::{
    qmp::Session,
    storage::{FileJob, FileRequest},
    HostOptions,
};
use std::{
    fs,
    path::PathBuf,
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

fn main() -> Result<()> {
    let mut args = std::env::args_os().skip(1);
    let engine = PathBuf::from(
        args.next()
            .context("Usage: phone-files-resume ENGINE COPIED_STATE CHECKPOINT")?,
    );
    let state = PathBuf::from(args.next().context("Copied prepared state required")?);
    let checkpoint = PathBuf::from(args.next().context("Starting checkpoint required")?);
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
        let mut session = Session::open(&engine, &state, Some(&checkpoint))?;
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
            resume: true,
        };
        let mut session = Session::start(&options, &state)?;
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
            "Imported file did not survive the checkpoint"
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
            "Restored export contents differ"
        );
        run(&mut session, FileRequest::Delete { path })?;
        println!(
            "{}",
            serde_json::json!({"phase":"after_resume","files":session.files.snapshot()})
        );
        session.save()?;
    }
    println!("{}", serde_json::json!({"save_resume_roundtrip":"passed"}));
    Ok(())
}
