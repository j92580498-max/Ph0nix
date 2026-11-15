// SPDX-License-Identifier: GPL-2.0-or-later
//! Exercise the same native app transport with real firmware and checkpoints.
use anyhow::{Context,Result};
use db2010_host::{prepare, qmp::{Session,poll_delay}, HostOptions};
use std::{fs::File,io::Write,path::PathBuf,thread,time::{Duration,Instant}};

fn drain_audio(session: &Session, output: &mut Option<File>) -> Result<()> {
    while let Some(packet) = session.audio.take() {
        if let Some(file) = output.as_mut() { file.write_all(&packet.pcm)?; }
    }
    Ok(())
}

fn advance_running(session: &mut Session, seconds: f64, audio: &mut Option<File>) -> Result<()> {
    let start = session.progress()?["virtual_ns"].as_u64().context("guest clock")?;
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        drain_audio(session, audio)?;
        let progress = session.progress()?;
        anyhow::ensure!(progress["running"].as_bool() == Some(true), "guest stopped: {}", session.summary()?);
        if progress["virtual_ns"].as_u64().context("guest clock")?.saturating_sub(start) as f64 >= seconds * 1e9 { break; }
        anyhow::ensure!(Instant::now() < deadline, "guest advance deadline");
        thread::sleep(Duration::from_millis(20));
    }
    Ok(())
}

fn advance(session: &mut Session, seconds: f64, audio: &mut Option<File>) -> Result<()> {
    session.qmp.execute("cont", serde_json::json!({}))?;
    advance_running(session, seconds, audio)?;
    session.qmp.execute("stop", serde_json::json!({}))?;
    Ok(())
}

fn capture(session: &mut Session, data: &std::path::Path, name: &str) -> Result<()> {
    std::fs::create_dir_all(data)?;
    let (suffix, status) = if session.running()? {
        ("progress", session.progress()?)
    } else { ("summary", session.summary()?) };
    std::fs::write(data.join(format!("{name}-{suffix}.json")),
        serde_json::to_vec_pretty(&status)?)?;
    if let Some(frame) = session.frame()? {
        std::fs::create_dir_all(data)?;
        std::fs::write(data.join(format!("{name}.rgb565")), &frame.rgb565_be)?;
        let mut ppm = format!("P6\n{} {}\n255\n", frame.width, frame.height).into_bytes();
        ppm.extend_from_slice(&frame.rgb());
        std::fs::write(data.join(format!("{name}.ppm")), ppm)?;
    }
    Ok(())
}

fn capture_failure_snapshot(session: &mut Session, data: &std::path::Path) -> Result<PathBuf> {
    // Full firmware snapshots can exceed the QMP frame bound. Keep each
    // authenticated engine chunk bounded and save its original envelope.
    session.qmp.execute("qom-set", serde_json::json!({"path":"/machine",
        "property":"snapshot-chunk","value":"start"}))?;
    let mut chunks = Vec::new();
    let mut offset = 0;
    loop {
        let chunk = session.qmp.property("/machine", "snapshot-chunk")?;
        let total = chunk["total"].as_u64().context("snapshot total")?;
        let encoded = chunk["data_base64"].as_str().context("snapshot data")?;
        anyhow::ensure!(total <= 16 * 1024 * 1024 && chunk["offset"].as_u64() == Some(offset),
            "snapshot exceeds bound or has a discontinuous cursor");
        anyhow::ensure!(!encoded.is_empty() && encoded.len() % 4 == 0, "invalid snapshot chunk");
        let padding = encoded.bytes().rev().take_while(|byte| *byte == b'=').count();
        anyhow::ensure!(padding <= 2, "invalid snapshot padding");
        let bytes = encoded.len() / 4 * 3 - padding;
        anyhow::ensure!(bytes > 0 && bytes <= 256 * 1024 && offset + bytes as u64 <= total,
            "invalid snapshot chunk size");
        offset += bytes as u64;
        chunks.push(chunk);
        if offset == total { break; }
    }
    let path = data.join("failure-snapshot-chunks.json");
    std::fs::write(&path, serde_json::to_vec(&chunks)?)?;
    Ok(path)
}

fn main() -> Result<()> {
    let args:Vec<String>=std::env::args().skip(1).collect();
    let get=|name:&str|args.iter().position(|a|a==name).and_then(|i|args.get(i+1)).map(PathBuf::from);
    let engine=get("--engine").context("--engine PATH")?;
    let data=get("--data-dir").unwrap_or_else(||PathBuf::from("build/native-check"));
    let mut prepared=if get("--state").is_none() {
        Some(prepare::prepare_locked(&get("--main").context("--main PATH")?,
            &get("--fs").context("--fs PATH")?,&data)?)
    } else {None};
    let state=get("--state").or_else(||prepared.as_ref().map(|value|value.state.clone())).context("prepared state")?;
    let seconds=args.iter().position(|a|a=="--seconds").and_then(|i|args.get(i+1))
        .map(|s|s.parse::<u64>()).transpose()?.unwrap_or(10);
    let mut session=if let Some(checkpoint)=get("--checkpoint") {
        match prepared.take() {Some(prepared)=>Session::open_prepared(&engine,prepared,&checkpoint)?,None=>Session::open(&engine,&state,Some(&checkpoint))?}
    } else {
        let options=HostOptions {engine,data_dir:data.clone(),resume:!args.iter().any(|arg|arg=="--no-resume")};
        match prepared.take() {Some(prepared)=>Session::start_prepared(&options,prepared)?,None=>Session::start(&options,&state)?}
    };
    let mut audio = if args.iter().any(|arg| arg == "--audio") {
        std::fs::create_dir_all(&data)?;
        Some(File::create(data.join("captured.pcm"))?)
    } else {None};
    if let Some(plan) = get("--plan") {
        let actions: serde_json::Value = serde_json::from_slice(&std::fs::read(plan)?)?;
        capture(&mut session, &data, "00-before")?;
        for action in actions.as_array().context("plan action array")? {
            let name = action["name"].as_str().context("action name")?;
            let keys: Vec<&str> = action["keys"].as_array().context("action keys")?
                .iter().map(|key|key.as_str().context("key name")).collect::<Result<_>>()?;
            session.contacts(&keys)?;
            for field in ["hold_guest_s", "settle_guest_s"] {
                let seconds = action[field].as_f64().context("action duration")?;
                anyhow::ensure!(seconds >= 0.0, "negative action duration");
                advance_running(&mut session, seconds, &mut audio)?;
                session.contacts(&[])?;
            }
            capture(&mut session, &data, name)?;
        }
    }
    if let Some(keys) = args.iter().position(|arg| arg == "--keys").and_then(|index| args.get(index + 1)) {
        session.qmp.execute("stop", serde_json::json!({}))?;
        capture(&mut session, &data, "00-before")?;
        for (index, key) in keys.split(',').enumerate() {
            session.contacts(&[key])?;
            advance(&mut session, 0.15, &mut audio)?;
            session.contacts(&[])?;
            advance(&mut session, 2.0, &mut audio)?;
            capture(&mut session, &data, &format!("{:02}-{key}", index + 1))?;
        }
        session.qmp.execute("cont", serde_json::json!({}))?;
    }
    let guest_start=session.progress()?["virtual_ns"].as_u64().context("guest clock")?;
    let start=Instant::now(); let mut due=start; let mut status_due=start; let mut polls=0; let mut frames=0;
    let mut max_poll=Duration::ZERO;
    while start.elapsed()<Duration::from_secs(seconds) {
        let call=Instant::now();
        if let Some(frame)=session.frame()? {
            frames+=1;
            std::fs::create_dir_all(&data)?;
            std::fs::write(data.join("lcd.rgb565"),&frame.rgb565_be)?;
        }
        max_poll=max_poll.max(call.elapsed()); polls+=1;
        drain_audio(&session, &mut audio)?;
        if Instant::now()>=status_due {
            let progress=session.progress()?;
            if progress["running"].as_bool()!=Some(true) {
                let snapshot = capture_failure_snapshot(&mut session, &data)
                    .map(|path| serde_json::json!({"chunks":path}))
                    .unwrap_or_else(|error| serde_json::json!({"error":format!("{error:#}")}));
                let failure=serde_json::json!({"progress":progress,"snapshot":snapshot,
                    "status":session.qmp.execute("query-status",serde_json::json!({}))?,
                    "summary":session.summary()?,
                    "diagnostics":session.diagnostics.snapshot()});
                std::fs::write(data.join("failure.json"),serde_json::to_vec_pretty(&failure)?)?;
                anyhow::bail!("guest stopped: {}; details: {}",failure["summary"],data.join("failure.json").display());
            }
            status_due=Instant::now()+Duration::from_millis(250);
        }
        let (delay,missed)=poll_delay(&mut due,Instant::now());
        if missed!=0 {session.diagnostics.record_missed_deadlines(missed);}
        thread::sleep(delay);
    }
    let progress=session.progress()?;
    let guest_seconds=(progress["virtual_ns"].as_u64().context("guest clock")?.saturating_sub(guest_start)) as f64/1e9;
    let sound=session.audio.stats();
    let result=serde_json::json!({"host_seconds":start.elapsed().as_secs_f64(),
        "guest_seconds":guest_seconds, "guest_speed":guest_seconds/start.elapsed().as_secs_f64(),
        "polls":polls,"poll_hz":polls as f64/start.elapsed().as_secs_f64(),
        "native_lcd_updates":frames,"max_poll_ms":max_poll.as_secs_f64()*1000.0,
        "progress":progress,"state":state,"diagnostics":session.diagnostics.snapshot(),
        "audio":{"frames":sound.frames,"nonzero_samples":sound.nonzero_samples,
            "dropped_frames":sound.dropped_frames,"invalid_connections":sound.invalid_connections}});
    std::fs::create_dir_all(&data)?;
    std::fs::write(data.join("result.json"),serde_json::to_vec_pretty(&result)?)?;
    session.qmp.execute("stop", serde_json::json!({}))?;
    std::fs::write(data.join("summary.json"),serde_json::to_vec_pretty(&session.summary()?)?)?;
    println!("{result}");
    if args.iter().any(|a|a=="--save") {session.save()?;}
    Ok(())
}
