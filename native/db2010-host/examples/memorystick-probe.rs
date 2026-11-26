// SPDX-License-Identifier: GPL-2.0-or-later
//! Exercise a persistent Memory Stick through stock K750i R1DB001 and UART OBEX.
use anyhow::{bail, ensure, Context, Result};
use db2010_host::{
    prepare,
    qmp::Session,
    storage::{CardImage, FileEntry, FileRequest},
};
use serde_json::{json, Value};
use std::{
    fs,
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

const SEED: &str = "SEED.TXT";
const WRITTEN: &str = "WRITTEN.TXT";

fn probe(session: &mut Session) -> Result<Value> {
    session.qmp.property("/machine", "memorystick-probe-status")
}

fn require_running(session: &mut Session) -> Result<()> {
    if !session.running()? {
        bail!("K750i stopped: {}; Memory Stick: {}", session.summary()?["stop"], probe(session)?);
    }
    Ok(())
}

fn boot(engine: &Path, state: &Path, first: bool) -> Result<Session> {
    let mut session = Session::open(engine, state, None)?;
    let seconds = if first { 240 } else { 150 };
    let deadline = Instant::now() + Duration::from_secs(seconds * 3 + 30);
    loop {
        let progress = session.progress()?;
        if progress["running"] != true {
            require_running(&mut session)?;
        }
        if progress["virtual_ns"].as_u64().unwrap_or(0) >= seconds * 1_000_000_000
            && progress["serial_input_ready"] == true
            && progress["serial_cable_present"] == true
        {
            break;
        }
        ensure!(Instant::now() < deadline, "K750i did not finish boot: {progress}");
        thread::sleep(Duration::from_millis(200));
    }
    if first {
        // Select the language, decline the setup wizard and dismiss its message.
        for key in ["soft-left", "soft-right", "soft-left"] {
            session.contacts(&[key])?;
            thread::sleep(Duration::from_millis(150));
            session.contacts(&[])?;
            thread::sleep(Duration::from_secs(3));
        }
    }
    Ok(session)
}

fn insert(session: &mut Session, card: &CardImage) -> Result<()> {
    let before = probe(session)?;
    ensure!(before["card_present"] == false && before["detect_irqs"] == 0, "Slot not empty: {before}");
    println!("Empty slot: {before}");
    session.qmp.execute("qom-set", json!({
        "path":"/machine", "property":"memorystick-image", "value":card.path()
    }))?;
    let lock_error = card.info().expect_err("Host opened the image while the engine held it");
    ensure!(lock_error.to_string().contains("in use"), "Unexpected image error: {lock_error:#}");
    let deadline = Instant::now() + Duration::from_secs(40);
    let mut last = Value::Null;
    let mut next_log = Instant::now();
    loop {
        require_running(session)?;
        let status = probe(session)?;
        if status != last && Instant::now() >= next_log {
            println!("Insertion: {status}");
            next_log = Instant::now() + Duration::from_secs(1);
            last = status.clone();
        }
        if status["attribute_sectors_read"].as_u64().unwrap_or(0) >= 2
            && status["sectors_read"].as_u64().unwrap_or(0) > 0
        {
            ensure!(status["card_present"] == true && status["detect_irqs"] == 1,
                "Unexpected card detect status: {status}");
            println!("K750i read sectors from the inserted card: {status}");
            return Ok(());
        }
        ensure!(Instant::now() < deadline, "K750i failed to read the card: {status}");
        thread::sleep(Duration::from_millis(50));
    }
}

fn request(session: &mut Session, operation: FileRequest) -> Result<()> {
    let description = serde_json::to_string(&operation)?;
    let deadline = Instant::now() + Duration::from_secs(45);
    while !session.files.snapshot().attached {
        require_running(session)?;
        ensure!(Instant::now() < deadline, "File transport did not attach");
        thread::sleep(Duration::from_millis(20));
    }
    let id = session.files.request(operation)?;
    loop {
        let snapshot = session.files.snapshot();
        if let Some(job) = snapshot.job {
            if job.id == id && !snapshot.busy {
                ensure!(job.phase == "complete", "File job {description}: {}", job.error.unwrap_or_default());
                return Ok(());
            }
        }
        require_running(session)?;
        ensure!(Instant::now() < deadline, "File job deadline: {description}");
        thread::sleep(Duration::from_millis(50));
    }
}

fn list(session: &mut Session, path: &[String]) -> Result<Vec<FileEntry>> {
    request(session, FileRequest::List { path: path.to_vec() })?;
    let entries = session.files.snapshot().entries;
    println!("Phone lists {}: {}", path.join("/"), serde_json::to_string(&entries)?);
    Ok(entries)
}

fn find_card_folder(session: &mut Session) -> Result<Vec<String>> {
    let root = list(session, &[])?;
    for entry in root.iter().filter(|entry| entry.directory) {
        let top = vec![entry.name.clone()];
        let folders = list(session, &top)?;
        for folder in folders.iter().filter(|e| e.directory && e.name.eq_ignore_ascii_case("Other")) {
            let path = vec![entry.name.clone(), folder.name.clone()];
            if list(session, &path)?.iter().any(|e| !e.directory && e.name.eq_ignore_ascii_case(SEED)) {
                return Ok(path);
            }
        }
    }
    bail!("K750i file service did not expose the seeded card file; Memory Stick: {}", probe(session)?)
}

fn export(session: &mut Session, path: Vec<String>, host: PathBuf, expected: &[u8]) -> Result<()> {
    request(session, FileRequest::Export { path, host: host.clone(), move_source: false })?;
    ensure!(fs::read(host)? == expected, "File read through firmware differs");
    Ok(())
}

fn main() -> Result<()> {
    let [engine, workspace, main, fs_image]: [PathBuf; 4] = std::env::args_os()
        .skip(1)
        .map(PathBuf::from)
        .collect::<Vec<_>>()
        .try_into()
        .ok()
        .context("Usage: memorystick-probe ENGINE WORKSPACE MAIN FS")?;
    let state = prepare::prepare_locked_at(&main, &fs_image, &workspace)?.state;
    let workspace = workspace.canonicalize()?;
    let card = CardImage::create(&workspace.join("card.img"), 32)?;
    let other = vec!["Other".into()];
    card.make_directory(&other)?;
    let seed = b"This file was placed on the card before boot.\r\n".repeat(140);
    let written = b"This file was written by the K750i firmware.\r\n".repeat(240);
    let seed_host = workspace.join("seed-input.txt");
    fs::write(&seed_host, &seed)?;
    card.import_file(&[other.clone(), vec![SEED.into()]].concat(), &seed_host, false, false)?;
    let write_host = workspace.join("write-input.txt");
    fs::write(&write_host, &written)?;

    let mut session = boot(&engine, &state, true)?;
    insert(&mut session, &card)?;
    let folder = find_card_folder(&mut session)?;
    export(&mut session, [folder.clone(), vec![SEED.into()]].concat(), workspace.join("readback.txt"), &seed)?;
    let path = [folder.clone(), vec![WRITTEN.into()]].concat();
    request(&mut session, FileRequest::Import { path: path.clone(), host: write_host, replace: false, move_source: false })?;
    let status = probe(&mut session)?;
    ensure!(status["sectors_written"].as_u64().unwrap_or(0) > 0, "No sector writes through the card controller: {status}");
    println!("After firmware write: {status}");
    session.save_with_resume(false)?;
    drop(session);

    let raw_readback = workspace.join("image-readback.txt");
    card.export_file(&[other, vec![WRITTEN.into()]].concat(), &raw_readback, false)?;
    ensure!(fs::read(raw_readback)? == written, "Firmware write did not persist in the card image");
    ensure!(!workspace.join("phone.saved.json").exists(), "Unexpected checkpoint in a cold-boot test");
    let mut session = boot(&engine, &state, false)?;
    insert(&mut session, &card)?;
    export(&mut session, path, workspace.join("cold-readback.txt"), &written)?;
    println!("Memory Stick read, write and cold-boot persistence passed");
    Ok(())
}
