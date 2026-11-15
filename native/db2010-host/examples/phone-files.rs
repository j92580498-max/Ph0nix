// SPDX-License-Identifier: GPL-2.0-or-later
//! A native firmware-backed OBEX check. Never edits the NOR image offline.
use anyhow::{ensure, Context, Result};
use db2010_host::{
    qmp::Session,
    storage::{FileJob, FileRequest},
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
        let progress = session.progress()?;
        ensure!(
            progress["running"].as_bool() == Some(true),
            "Firmware stopped during file operation"
        );
        thread::sleep(Duration::from_millis(50));
    }
}
fn main() -> Result<()> {
    let mut args = std::env::args_os().skip(1);
    let engine = PathBuf::from(args.next().context(
        "Usage: phone-files ENGINE STATE CHECKPOINT [PHONE_DIRECTORY/...] [UPLOAD_SOURCE]",
    )?);
    let state = PathBuf::from(args.next().context("Prepared state JSON required")?);
    let checkpoint = PathBuf::from(args.next().context("Checkpoint required")?);
    let directory: Vec<String> = args
        .next()
        .map(|x| {
            x.to_string_lossy()
                .split('/')
                .filter(|p| !p.is_empty())
                .map(String::from)
                .collect()
        })
        .unwrap_or_default();
    let source = args.next().map(PathBuf::from);
    let mut session = Session::open(&engine, &state, Some(&checkpoint))?;
    thread::sleep(Duration::from_millis(250));
    println!("{}", serde_json::to_string(&session.progress()?)?);
    run(
        &mut session,
        FileRequest::List {
            path: directory.clone(),
        },
    )?;
    println!("{}", serde_json::to_string(&session.files.snapshot())?);
    if let Some(source) = source {
        let mut path = directory;
        path.push("ph0nix-transfer-test.txt".into());
        run(
            &mut session,
            FileRequest::Import {
                path: path.clone(),
                host: source.clone(),
                replace: false,
                move_source: false,
            },
        )?;
        let output = state
            .parent()
            .context("State directory missing")?
            .join("ph0nix-transfer-received.txt");
        run(
            &mut session,
            FileRequest::Export {
                path: path.clone(),
                host: output.clone(),
                move_source: false,
            },
        )?;
        ensure!(
            fs::read(&source)? == fs::read(&output)?,
            "Roundtrip contents differ"
        );
        run(&mut session, FileRequest::Delete { path })?;
    }
    run(&mut session, FileRequest::Disconnect)?;
    session.qmp.execute("stop", serde_json::json!({}))?;
    println!("{}", serde_json::to_string(&session.summary()?)?);
    Ok(())
}
