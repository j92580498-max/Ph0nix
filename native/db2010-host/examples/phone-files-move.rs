// SPDX-License-Identifier: GPL-2.0-or-later
//! Verify imported moves commit before removing the host source; also inject
//! a metadata failure and verify the healthy checkpoint and source survive.
use anyhow::{ensure, Context, Result};
use db2010_host::{
    qmp::Session,
    storage::{FileJob, FileRequest},
    HostOptions,
};
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::PathBuf,
    thread,
    time::{Duration, Instant},
};

fn wait(session: &mut Session, id: u64) -> Result<FileJob> {
    let deadline = Instant::now() + Duration::from_secs(45);
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
            "Firmware stopped"
        );
        thread::sleep(Duration::from_millis(20));
    }
}
fn request(session: &mut Session, request: FileRequest) -> Result<FileJob> {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !session.files.snapshot().attached {
        ensure!(Instant::now() < deadline, "File transport did not attach");
        thread::sleep(Duration::from_millis(10));
    }
    let id = session.files.request(request)?;
    wait(session, id)
}
fn main() -> Result<()> {
    let mut args = std::env::args_os().skip(1);
    let engine = PathBuf::from(
        args.next()
            .context("Usage: phone-files-move ENGINE COPIED_STATE CHECKPOINT [fail-save]")?,
    );
    let state = PathBuf::from(args.next().context("Copied state required")?);
    let checkpoint = PathBuf::from(args.next().context("Checkpoint required")?);
    let fail = args.next().is_some_and(|arg| arg == "fail-save");
    let workspace = state
        .parent()
        .context("Prepared workspace required")?
        .to_owned();
    let source = workspace.join("move-source.txt");
    let contents = b"ph0nix imported move must commit before source removal\n";
    fs::write(&source, contents)?;
    let directory = vec!["Phone memory".to_owned(), "Other".to_owned()];
    let mut path = directory.clone();
    path.push("ph0nix-move-test.txt".into());
    {
        let mut session = Session::open(&engine, &state, Some(&checkpoint))?;
        request(
            &mut session,
            FileRequest::List {
                path: directory.clone(),
            },
        )?;
        session.save()?;
        session.resume_after_save()?;
        let healthy = fs::read(workspace.join("phone.saved.json"))?;
        let id = session.files.request(FileRequest::Import {
            path: path.clone(),
            host: source.clone(),
            replace: false,
            move_source: true,
        })?;
        let deadline = Instant::now() + Duration::from_secs(45);
        while !session.files.pending_commit() {
            ensure!(
                Instant::now() < deadline,
                "Import did not reach pending commit"
            );
            ensure!(
                session.progress()?["running"].as_bool() == Some(true),
                "Firmware stopped"
            );
            thread::sleep(Duration::from_millis(20));
        }
        ensure!(
            source.is_file(),
            "Source disappeared before durable checkpoint"
        );
        println!(
            "{}",
            serde_json::json!({"phase":"awaiting_save","source_exists":true,"job":session.files.snapshot().job})
        );
        if fail {
            let fault = workspace.join("phone.saved.json.pending");
            fs::create_dir(&fault)?;
            let result = session.save();
            ensure!(
                result.is_err(),
                "Injected metadata failure was not detected"
            );
            ensure!(
                source.is_file(),
                "Failed checkpoint deleted the original source"
            );
            ensure!(
                fs::read(workspace.join("phone.saved.json"))? == healthy,
                "Failed checkpoint replaced the healthy metadata"
            );
            let snapshot = session.files.snapshot();
            ensure!(
                !snapshot.busy
                    && snapshot
                        .job
                        .as_ref()
                        .is_some_and(|job| job.id == id && job.phase == "error"),
                "Failed import did not retain its original error job"
            );
            println!(
                "{}",
                serde_json::json!({"phase":"save_failure","source_exists":true,"previous_checkpoint_kept":true,"job":snapshot.job})
            );
            fs::remove_dir(fault)?;
            session.resume_after_save()?;
        } else {
            session.save()?;
            ensure!(
                !source.exists(),
                "Source was not removed after durable checkpoint"
            );
            let job = session
                .files
                .snapshot()
                .job
                .context("Completed import job missing")?;
            ensure!(
                job.id == id && job.phase == "complete",
                "Durable commit changed or lost the original job"
            );
            println!(
                "{}",
                serde_json::json!({"phase":"committed","source_exists":false,"job":job})
            );
            session.resume_after_save()?;
        }
    }
    let options = HostOptions {
        engine,
        data_dir: workspace.clone(),
        resume: true,
    };
    let mut session = Session::start(&options, &state)?;
    request(&mut session, FileRequest::List { path: directory })?;
    let present = session
        .files
        .snapshot()
        .entries
        .iter()
        .any(|entry| entry.name == "ph0nix-move-test.txt");
    ensure!(
        present != fail,
        "Restored checkpoint contains the wrong file state"
    );
    if !fail {
        let output = workspace.join("move-received.txt");
        request(
            &mut session,
            FileRequest::Export {
                path: path.clone(),
                host: output.clone(),
                move_source: false,
            },
        )?;
        ensure!(
            fs::read(output)? == contents,
            "Restored moved file contents differ"
        );
        let sha256 = Sha256::digest(contents)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        request(
            &mut session,
            FileRequest::DeleteVerified {
                path,
                bytes: contents.len() as u64,
                sha256,
            },
        )?;
    } else {
        ensure!(
            fs::read(&source)? == contents,
            "Original source contents changed after failure"
        );
    }
    session.save()?;
    println!(
        "{}",
        serde_json::json!({"deferred_move":"passed","injected_save_failure":fail})
    );
    Ok(())
}
