// SPDX-License-Identifier: GPL-2.0-or-later
//! Boot stock K750i firmware and probe its first Memory Stick transfer.
use anyhow::{bail, ensure, Context, Result};
use db2010_host::{prepare, qmp::Session};
use serde_json::json;
use std::{
    path::PathBuf,
    thread,
    time::{Duration, Instant},
};

fn main() -> Result<()> {
    let [engine, workspace, main, fs]: [PathBuf; 4] = std::env::args_os()
        .skip(1)
        .map(PathBuf::from)
        .collect::<Vec<_>>()
        .try_into()
        .ok()
        .context("Usage: memorystick-probe ENGINE WORKSPACE MAIN FS")?;
    let state = prepare::prepare_locked_at(&main, &fs, &workspace)?.state;
    let mut session = Session::open(&engine, &state, None)?;
    let deadline = Instant::now() + Duration::from_secs(750);
    loop {
        let progress = session.progress()?;
        if progress["running"].as_bool() != Some(true) {
            bail!("K750i stopped with an empty slot: {}", session.summary()?);
        }
        if progress["virtual_ns"].as_u64().unwrap_or(0) >= 240_000_000_000
            && progress["serial_input_ready"].as_bool() == Some(true)
        {
            break;
        }
        ensure!(Instant::now() < deadline, "K750i did not finish boot: {progress}");
        thread::sleep(Duration::from_millis(400));
    }
    let before = session.qmp.property("/machine", "memorystick-probe-status")?;
    ensure!(before["card_present"] == false && before["detect_irqs"] == 0);
    println!("Empty slot: {before}");
    session.qmp.execute(
        "qom-set",
        json!({"path":"/machine", "property":"memorystick-probe-inserted", "value":true}),
    )?;
    let after = session.qmp.property("/machine", "memorystick-probe-status")?;
    println!("Insertion: {after}");
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if !session.running()? {
            let probe = session.qmp.property("/machine", "memorystick-probe-status")?;
            let summary = session.summary()?;
            println!("Probe: {probe}");
            println!("Stop: {}", summary["stop"]);
            ensure!(summary["stop"]["reason"] == "memorystick_probe_first_command",
                "Firmware stopped before the first TPC");
            ensure!(probe["detect_irqs"].as_u64().unwrap_or(0) > 0,
                "GPIO4 did not deliver the insertion interrupt");
            let command = probe["first_command"].as_u64().context("Missing TPC")?;
            ensure!(command >> 12 != 0, "Invalid TPC command {command:#06x}");
            println!("K750i Memory Stick first command: {command:#06x}");
            return Ok(());
        }
        if Instant::now() >= deadline {
            let probe = session.qmp.property("/machine", "memorystick-probe-status")?;
            bail!("K750i did not issue a Memory Stick command: {probe}");
        }
        thread::sleep(Duration::from_millis(100));
    }
}
