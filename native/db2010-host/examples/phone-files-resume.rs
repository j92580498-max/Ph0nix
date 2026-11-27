// SPDX-License-Identifier: GPL-2.0-or-later
//! Check native file transfer across resume or a cold boot from a portable memory file.
use anyhow::{ensure, Context, Result};
use db2010_host::{
    prepare,
    qmp::{export_internal_memory, import_internal_memory, Session},
    storage::{internal::{self, InUse, Request}, internal_other_path, FileEntry, FileJob, FileRequest},
    HostOptions, LcdFrame,
};
use std::{
    fs,
    io::{Read, Seek, SeekFrom, Write},
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
        if session.files.pending_commit() {
            session.save_with_resume(false)?;
            session.resume_after_save()?;
        }
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

fn internal_other(session: &mut Session) -> Result<Vec<String>> {
    internal_other_path(|path| list(session, path.to_vec()))
}

fn assert_no_checkpoint(workspace: &Path) -> Result<()> {
    for entry in fs::read_dir(workspace)? {
        let name = entry?.file_name();
        let name = name.to_string_lossy();
        ensure!(name != "phone.saved.json" && !name.ends_with(".vmstate"),
            "Memory-only save unexpectedly created {name}");
    }
    Ok(())
}

fn assert_internal_locked(workspace: &Path) -> Result<()> {
    for request in [Request::Info { path: vec![] }, Request::List { path: vec![] },
        Request::MakeDirectory { path: vec!["must-not-be-created".into()] }] {
        let error = internal::request(workspace, request).expect_err("In-use memory was accessible");
        ensure!(error.is::<InUse>(), "Expected an in-use error, got {error:#}");
    }
    Ok(())
}

fn verify_pause(session: &mut Session, workspace: &Path) -> Result<()> {
    assert_internal_locked(workspace)?;
    session.pause()?;
    assert_internal_locked(workspace)?;
    ensure!(!session.running()?, "Paused VM still reports running");
    let frozen = session.progress()?;
    thread::sleep(Duration::from_millis(750));
    let later = session.progress()?;
    // The real-time engine does not expose an instruction counter.
    for field in ["virtual_ns", "pc", "timer_ticks", "timer_irqs"] {
        ensure!(!frozen[field].is_null() && frozen[field] == later[field], "{field} changed while paused");
    }
    session.resume()?;
    let deadline = Instant::now() + Duration::from_secs(5);
    while session.progress()?["virtual_ns"].as_u64() <= frozen["virtual_ns"].as_u64() {
        ensure!(Instant::now() < deadline, "Guest clock did not advance after resume");
        thread::sleep(Duration::from_millis(20));
    }
    ensure!(session.running()?, "VM did not return to running after resume");
    println!("{}", serde_json::json!({"in_memory_pause_resume":"passed"}));
    Ok(())
}

fn verify_offline(workspace: &Path, other_phone: &Path, source: &Path) -> Result<()> {
    let old_path = vec!["other".into(), "ph0nix-resume-test.txt".into()];
    let exported = workspace.join("offline-received.txt");
    internal::request(workspace, Request::Export { path: old_path, host: exported.clone() })?;
    ensure!(fs::read(&exported)? == fs::read(source)?, "Offline export of the firmware's file differs");
    fs::remove_file(exported)?;

    let path = vec!["image".into(), "ph0nix-offline.txt".into()];
    let other_generation = memory_generation(other_phone)?;
    let before = internal::request(workspace, Request::Info { path: vec![] })?;
    internal::request(workspace, Request::Import {
        path: path.clone(), host: source.to_owned(), replace: false, generation: None,
    })?;
    let after = memory_generation(workspace)?;
    ensure!(other_generation == memory_generation(other_phone)?, "Offline import changed the second phone");
    let old_generation = before["generation"].as_str().context("Missing memory generation")?;
    let error = internal::request(workspace, Request::Import {
        path, host: source.to_owned(), replace: true, generation: Some(old_generation.into()),
    }).expect_err("An obsolete writer replaced a file");
    ensure!(error.to_string().contains("memory changed") && after == memory_generation(workspace)?,
        "Unexpected stale writer result: {error:#}");
    println!("{{\"offline_memory_write\":\"passed\"}}");
    Ok(())
}

fn memory_generation(workspace: &Path) -> Result<[u8; 24]> {
    let mut header = [0; 24];
    fs::File::open(workspace.join("phone-memory.k750mem"))?.read_exact(&mut header)?;
    Ok(header)
}

fn main() -> Result<()> {
    let mut args = std::env::args_os().skip(1);
    let engine = PathBuf::from(args.next().context(
        "Usage: phone-files-resume ENGINE COPIED_STATE CHECKPOINT [cold-boot] | ENGINE --fresh DATA_DIR MAIN FS",
    )?);
    let input = args.next().context("Copied prepared state or --fresh required")?;
    let (state, checkpoint, cold_boot, fresh) = if input == "--fresh" {
        let data = PathBuf::from(args.next().context("New data directory required")?);
        let main = PathBuf::from(args.next().context("K750i MAIN required")?);
        let fs_image = PathBuf::from(args.next().context("K750i FS required")?);
        ensure!(!data.exists(), "Fresh test requires a new data directory");
        fs::create_dir_all(&data)?;
        let first = data.join("first-phone");
        let second = data.join("second-phone");
        prepare::create_internal_memory(&main, &fs_image, &first)?;
        prepare::create_internal_memory(&main, &fs_image, &second)?;
        assert_no_checkpoint(&first)?;
        assert_no_checkpoint(&second)?;
        let second_initial = memory_generation(&second)?;
        ensure!(memory_generation(&first)? != second_initial, "New phones share a memory generation");
        ensure!(prepare::create_internal_memory(&main, &fs_image, &first).is_err(),
            "Creating a phone over an existing workspace succeeded");
        let state = prepare::prepare_locked_at(&main, &fs_image, &first)?.state;
        (state, None, true, Some((second, second_initial, main, fs_image)))
    } else {
        let checkpoint = PathBuf::from(args.next().context("Starting checkpoint required")?);
        let cold_boot = match args.next() {
            None => false,
            Some(mode) => {
                ensure!(mode == "cold-boot", "Unknown boot mode");
                true
            }
        };
        (PathBuf::from(input), Some(checkpoint), cold_boot, None)
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
    let inbox = std::env::var_os("PH0NIX_TEST_ANDROID_INBOX").is_some();
    let offline = std::env::var_os("PH0NIX_TEST_ANDROID_OFFLINE").is_some();
    let staged = workspace.join("ph0nix-resume-test.txt");
    if inbox {
        ensure!(fresh.is_some(), "The Android inbox test requires a fresh phone");
        // A move from Android Files owns a staged copy in the inbox.
        fs::copy(&source, &staged)?;
    }
    let directory;
    let path;
    {
        println!("{}", serde_json::json!({"phase":"initial_boot","state":state}));
        let mut session = Session::open(&engine, &state, checkpoint.as_deref())?;
        let mut imported = false;
        if inbox {
            wait_for_serial(&mut session, &workspace, "inbox-early", 0)?;
            capture_panel(&mut session, "inbox-early")?;
            match run(&mut session, FileRequest::ImportInternal {
                name: "ph0nix-resume-test.txt".into(), host: staged.clone(),
            }) {
                Ok(_) => imported = true,
                Err(error) => {
                    eprintln!("Early Android inbox: {error:#}");
                    if error.to_string().contains("Restart K750i") {
                        report_boot_failure(&mut session, &workspace, "inbox-early");
                        drop(session);
                        session = Session::open(&engine, &state, None)?;
                    }
                }
            }
        }
        if checkpoint.is_none() {
            wait_for_serial(&mut session, &workspace, "initial-boot", 240)?;
            finish_first_setup(&mut session)?;
        }
        if fresh.is_some() { verify_pause(&mut session, &workspace)?; }
        if inbox && !imported {
            if let Err(error) = run(&mut session, FileRequest::ImportInternal {
                name: "ph0nix-resume-test.txt".into(), host: staged.clone(),
            }) {
                report_boot_failure(&mut session, &workspace, "inbox-after-setup");
                return Err(error);
            }
        }
        if inbox {
            ensure!(!staged.exists(), "Android inbox source was not released after a successful save");
            println!("{{\"android_inbox_import\":\"passed\",\"imported_before_setup\":{imported}}}");
        }
        directory = match internal_other(&mut session) {
            Ok(directory) => directory,
            Err(error) => {
                report_boot_failure(&mut session, &workspace, "initial-boot");
                return Err(error);
            }
        };
        path = [directory.clone(), vec!["ph0nix-resume-test.txt".into()]].concat();
        if !inbox {
            run(
                &mut session,
                FileRequest::Import {
                    path: path.clone(),
                    host: source.clone(),
                    replace: false,
                    move_source: false,
                },
            )?;
        }
        println!(
            "{}",
            serde_json::json!({"phase":"before_save","files":session.files.snapshot()})
        );
        // Deliberately leave the file service connected; Session owns safe quiescence.
        session.save_with_resume(!cold_boot)?;
        if fresh.is_some() { assert_internal_locked(&workspace)?; }
    }
    if offline {
        let (other, ..) = fresh.as_ref().context("Offline memory test requires a fresh phone")?;
        verify_offline(&workspace, other, &source)?;
    }
    let state = if cold_boot {
        if fresh.is_some() {
            assert_no_checkpoint(&workspace)?;
        }
        let backup = workspace.join("exported.k750mem");
        export_internal_memory(&workspace, &backup)?;
        let restored = if let Some((target, initial, main, fs_image)) = fresh {
            ensure!(memory_generation(&target)? == initial, "Saving the first phone changed the second phone");
            let original = memory_generation(&workspace)?;
            import_internal_memory(&target, &backup)?;
            ensure!(memory_generation(&target)? != initial, "Import did not replace the selected phone memory");
            ensure!(memory_generation(&workspace)? == original, "Import changed the first phone");
            let state = prepare::prepare_locked_at(&main, &fs_image, &target)?.state;
            ensure!(state.parent() == Some(target.as_path()), "Restore and boot chose different workspaces");
            assert_no_checkpoint(&target)?;
            println!("{}", serde_json::json!({"separate_memory_files_for_identical_firmware":"passed"}));
            state
        } else {
            import_internal_memory(&workspace, &backup)?;
            state
        };
        let target = restored.parent().context("Restored workspace required")?;
        let before = memory_generation(target)?;
        // Corrupt the final NAND spare byte without changing the file size or header.
        let mut corrupt = fs::OpenOptions::new().read(true).write(true).open(&backup)?;
        corrupt.seek(SeekFrom::End(-1))?;
        let mut byte = [0];
        corrupt.read_exact(&mut byte)?;
        corrupt.seek(SeekFrom::End(-1))?;
        corrupt.write_all(&[byte[0] ^ 1])?;
        drop(corrupt);
        let failure = import_internal_memory(target, &backup).expect_err("Corrupt memory file was accepted");
        ensure!(failure.to_string().contains("checksum"), "Unexpected import failure: {failure:#}");
        ensure!(memory_generation(target)? == before, "Failed import replaced the good memory file");
        fs::remove_file(backup)?;
        println!("{}", serde_json::json!({"memory_file_export_import":"passed","corrupt_import_rejected":true}));
        restored
    } else {
        state
    };
    {
        let workspace = state.parent().context("Boot workspace required")?.to_owned();
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
            fs::read(output)? == fs::read(&source)?,
            "Export contents after restart differ"
        );
        if offline {
            let root = directory.get(..directory.len().saturating_sub(1)).unwrap_or_default();
            let pictures = list(&mut session, root.to_vec())?.into_iter().find(|entry|
                entry.directory && matches!(entry.name.as_str(), "Pictures" | "Изображения"))
                .context("Missing K750i Pictures directory")?.name;
            let mut path = root.to_vec();
            path.extend([pictures, "ph0nix-offline.txt".into()]);
            let output = workspace.join("offline-after-boot.txt");
            run(&mut session, FileRequest::Export { path, host: output.clone(), move_source: false })?;
            ensure!(fs::read(output)? == fs::read(&source)?, "K750i did not read the offline file after boot");
            println!("{{\"offline_memory_firmware_readback\":\"passed\"}}");
        }
        run(&mut session, FileRequest::Delete { path })?;
        println!(
            "{}",
            serde_json::json!({"phase":"after_restart","files":session.files.snapshot()})
        );
        session.save_with_resume(!cold_boot)?;
        if cold_boot && checkpoint.is_none() {
            assert_no_checkpoint(&workspace)?;
        }
    }
    if cold_boot {
        println!("{}", serde_json::json!({"save_cold_boot_roundtrip":"passed"}));
    } else {
        println!("{}", serde_json::json!({"save_resume_roundtrip":"passed"}));
    }
    Ok(())
}
