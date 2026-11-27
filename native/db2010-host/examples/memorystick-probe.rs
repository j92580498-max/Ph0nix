// SPDX-License-Identifier: GPL-2.0-or-later
//! Exercise a persistent Memory Stick through stock K750i R1DB001 and UART OBEX.
use anyhow::{bail, ensure, Context, Result};
use db2010_host::{
    prepare, HostOptions,
    qmp::Session,
    storage::{CardImage, FileEntry, FileRequest},
};
use serde_json::{json, Value};
use std::{
    fs,
    path::PathBuf,
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

fn boot(options: &HostOptions, prepared: prepare::Prepared, card: &CardImage, first: bool) -> Result<Session> {
    println!("Starting {} K750i boot", if first { "first" } else { "cold" });
    let mut session = Session::start_prepared_with_card(
        options, prepared, Default::default(), Default::default(), Default::default(), Some(card.path()),
    )?;
    ensure!(!session.will_save_checkpoint(options.resume), "A card session must cold boot next time");
    let lock_error = card.info().expect_err("Host opened the image while the engine held it");
    ensure!(lock_error.to_string().contains("in use"), "Unexpected image error: {lock_error:#}");
    let mut last = Value::Null;
    let mut next_log = Instant::now();
    let seconds = if first { 240 } else { 150 };
    let deadline = Instant::now() + Duration::from_secs(seconds * 3 + 30);
    loop {
        let progress = session.progress()?;
        if progress["running"] != true {
            require_running(&mut session)?;
        }
        if Instant::now() >= next_log {
            let status = probe(&mut session)?;
            if status != last {
                println!("Card at boot ({} ns): {status}", progress["virtual_ns"]);
                last = status;
            }
            next_log = Instant::now() + Duration::from_secs(1);
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
    let status = probe(&mut session)?;
    if !(status["attribute_sectors_read"].as_u64().unwrap_or(0) >= 2
        && status["sectors_read"].as_u64().unwrap_or(0) > 0)
    {
        log_detect_state(&mut session, "After boot");
        bail!("K750i failed to read the card during boot: {status}");
    }
    ensure!(status["card_present"] == true && status["detect_irqs"] == 0,
        "Unexpected card detect status: {status}");
    println!("K750i read Memory Stick sectors at boot: {status}");
    Ok(session)
}

// These addresses belong to the stock R1DB001 MAIN used by this probe.
fn log_detect_state(session: &mut Session, phase: &str) {
    for (field, address, count) in [("detect task", 0x4c07add8, 12), ("GPIO42 subscription", 0x4c09937c, 32)] {
        let report = session.qmp.execute("human-monitor-command", json!({
            "command-line": format!("xp /{count}bx {address:#x}")
        }));
        match report {
            Ok(value) => println!("{phase} {field}: {}", value.as_str().unwrap_or_default().trim()),
            Err(error) => println!("{phase} {field}: {error:#}"),
        }
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
    let folder = list(session, &[])?.into_iter()
        .find(|entry| entry.directory && entry.name.eq_ignore_ascii_case("Memory Stick"))
        .context("K750i file service did not expose the Memory Stick")?;
    let path = vec![folder.name];
    ensure!(list(session, &path)?.iter().any(|entry| !entry.directory && entry.name.eq_ignore_ascii_case(SEED)),
        "K750i did not list the seed file at the root of the Memory Stick; card: {}", probe(session)?);
    Ok(path)
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
    fs::create_dir_all(&workspace)?;
    let workspace = workspace.canonicalize()?;
    let options = HostOptions { engine, data_dir: workspace.clone(), resume: true };
    let card = CardImage::create(&workspace.join("card.img"), 32)?;
    let seed = b"This file was placed on the card before boot.\r\n".repeat(140);
    let written = b"This file was written by the K750i firmware.\r\n".repeat(240);
    let seed_host = workspace.join("seed-input.txt");
    fs::write(&seed_host, &seed)?;
    card.import_file(&[SEED.into()], &seed_host, false, false)?;
    let write_host = workspace.join("write-input.txt");
    fs::write(&write_host, &written)?;

    let prepared = prepare::prepare_locked_at(&main, &fs_image, &workspace)?;
    let mut session = boot(&options, prepared, &card, true)?;
    let folder = match find_card_folder(&mut session) {
        Ok(folder) => folder,
        Err(error) => {
            drop(session);
            println!("Card root after stopping the engine: {:?}", card.list(&[]));
            return Err(error);
        }
    };
    export(&mut session, [folder.clone(), vec![SEED.into()]].concat(), workspace.join("readback.txt"), &seed)?;
    let path = [folder.clone(), vec![WRITTEN.into()]].concat();
    let before = probe(&mut session)?["sectors_written"].as_u64().context("Missing card write counter")?;
    request(&mut session, FileRequest::Import { path: path.clone(), host: write_host, replace: false, move_source: false })?;
    let status = probe(&mut session)?;
    ensure!(status["sectors_written"].as_u64().unwrap_or(0) > before, "No sector writes through the card controller since import (was {before}): {status}");
    println!("After firmware write: {status}");
    // A connected card has its own state and must survive a normal stop
    // even when the launcher is configured to resume the phone session.
    list(&mut session, &folder).context("Listing card files after the firmware import")?;
    session.files.quiesce(Duration::from_secs(20)).context("Disconnecting OBEX after the card write")?;
    println!("Card after OBEX disconnect: {}", probe(&mut session)?);
    // Check whether the firmware writes its cached directory and FAT after
    // the transfer session ends and before the host stops the CPU.
    let idle = Duration::from_secs(30);
    let until = Instant::now() + idle;
    while Instant::now() < until {
        require_running(&mut session)?;
        thread::sleep(Duration::from_millis(250));
    }
    println!("Card after {idle:?} of idle time: {}", probe(&mut session)?);
    session.save_with_resume(options.resume).context("Saving the first card session")?;
    drop(session);

    let root = card.list(&[]).context("Opening the raw card after stopping the engine")?;
    println!("Raw card root after stopping the engine: {}", serde_json::to_string(&root)?);
    let raw_readback = workspace.join("image-readback.txt");
    let raw_result = (|| -> Result<()> {
        card.export_file(&[WRITTEN.into()], &raw_readback, false)
            .context("Extracting the imported file from the raw FAT16 root")?;
        ensure!(fs::read(raw_readback).context("Reading the extracted file")? == written,
            "Firmware write did not persist in the card image");
        Ok(())
    })();
    if let Err(error) = &raw_result {
        println!("Raw image readback failed: {error:#}");
        for dirs in [&["MP3"][..], &["MSSEMC"], &["MSSEMC", "Media files"],
            &["MSSEMC", "Media files", "other"]] {
            let parts = dirs.iter().map(|part| (*part).into()).collect::<Vec<String>>();
            println!("Raw card {}: {:?}", dirs.join("/"), card.list(&parts));
        }
    }
    ensure!(!workspace.join("phone.saved.json").exists(), "Unexpected checkpoint in a cold-boot test");
    let prepared = prepare::prepare_locked_at(&main, &fs_image, &workspace)
        .context("Preparing a cold boot with the saved internal memory")?;
    let mut session = boot(&options, prepared, &card, false)?;
    list(&mut session, &folder).context("Listing card files after the cold boot")?;
    export(&mut session, path, workspace.join("cold-readback.txt"), &written)
        .context("Reading the imported file through firmware after the cold boot")?;
    raw_result?;
    println!("Memory Stick read, write and cold-boot persistence passed");
    Ok(())
}
