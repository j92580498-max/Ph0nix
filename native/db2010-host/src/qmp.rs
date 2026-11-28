// SPDX-License-Identifier: GPL-2.0-or-later
//! Loopback QMP works on Windows, Linux and packaged Android PIE engines.
use anyhow::{bail, ensure, Context, Result};
use serde_json::{json, Value};
use std::{fs, io::{BufRead, BufReader, Read, Seek, SeekFrom, Write}, net::{TcpListener,TcpStream},
    path::{Path,PathBuf}, process::{Child,Command,Stdio}, thread, time::{Duration,Instant}};
use crate::{flash::{self, FlashExport, PendingMemory, MEMORY_FILE}, HostOptions, LcdFrame};
use crate::audio::{AudioBuffer, AudioTransport};
use crate::diagnostics::DiagnosticsHandle;
use crate::display::DisplayTransport;
use crate::storage::{CardImage,PhoneFiles,SerialTransport};
use serde::{Deserialize,Serialize};

pub const PRESENTATION_INTERVAL:Duration=Duration::from_nanos(16_666_667);

/// Keep the 60 Hz cadence through an occasional slow full-panel reply. Retain
/// at most one overdue interval, so the next cheap metadata poll can catch up
/// without building an unlimited queue after a long stall.
pub fn poll_delay(due:&mut Instant,now:Instant) -> (Duration,u64) {
    *due+=PRESENTATION_INTERVAL;
    let late=now.saturating_duration_since(*due);
    let period_ns=PRESENTATION_INTERVAL.as_nanos();
    let missed=late.as_nanos().div_ceil(period_ns).min(u64::MAX as u128) as u64;
    if late>=PRESENTATION_INTERVAL {
        *due+=late-Duration::from_nanos((late.as_nanos()%period_ns) as u64);
    }
    (due.saturating_duration_since(now),missed)
}

pub struct Qmp { reader:BufReader<TcpStream>, request:Vec<u8>, response:String, sequence:u64,
    received_bytes:u64, diagnostics:DiagnosticsHandle, pub stopped:bool }
impl Qmp {
    #[cfg(test)]
    fn connect(port:u16) -> Result<Self> {
        Self::connect_with_diagnostics(port,DiagnosticsHandle::default())
    }
    fn connect_with_diagnostics(port:u16,diagnostics:DiagnosticsHandle) -> Result<Self> {
        let socket=TcpStream::connect(("127.0.0.1",port))?;
        socket.set_nodelay(true)?;
        socket.set_read_timeout(Some(Duration::from_secs(5)))?;
        socket.set_write_timeout(Some(Duration::from_secs(5)))?;
        // Complete LCD replies are about 155 KiB. A default 8 KiB reader
        // makes many short socket reads per update, particularly costly on
        // Android/proot hosts. Read-ahead stays bounded and is reused.
        let mut qmp=Self {reader:BufReader::with_capacity(64*1024,socket),request:Vec::with_capacity(256),response:String::with_capacity(1024),sequence:0,
            received_bytes:0,diagnostics,stopped:false};
        ensure!(qmp.receive()?.get("QMP").is_some(),"missing QMP greeting");
        qmp.execute("qmp_capabilities",json!({}))?; Ok(qmp)
    }
    fn receive(&mut self) -> Result<Value> {
        self.response.clear();
        let receive_start=Instant::now();
        let received=self.reader.read_line(&mut self.response);
        self.diagnostics.record_qmp_receive(receive_start.elapsed());
        ensure!(received?>0,"native engine disconnected");
        ensure!(self.response.len()<=512*1024,"QMP observation is too large");
        self.received_bytes=self.received_bytes.saturating_add(self.response.len() as u64);
        let decode_start=Instant::now();
        let reply=serde_json::from_str::<Value>(&self.response);
        self.diagnostics.record_qmp_decode(decode_start.elapsed());
        Ok(reply?)
    }
    pub fn execute(&mut self,command:&str,arguments:Value) -> Result<Value> {
        let started=Instant::now();
        let received=self.received_bytes;
        self.sequence+=1;
        let request=json!({"execute":command,"arguments":arguments,"id":self.sequence});
        let result=(|| {
        // Serialize before writing so JSON tokens do not become individual
        // TCP writes. Reuse the buffer for the 60 Hz observation stream.
        self.request.clear();
        serde_json::to_writer(&mut self.request,&request)?;
        self.request.push(b'\n');
        self.reader.get_mut().write_all(&self.request)?;
        loop {
            let mut reply=self.receive()?;
            if let Some(event)=reply.get("event") {
                if event=="STOP" {self.stopped=true;self.diagnostics.set_running(false);}
                else if event=="RESUME" {self.stopped=false;self.diagnostics.set_running(true);}
                self.diagnostics.log("info","engine",format!("QMP event: {event}"));
                continue;
            }
            ensure!(reply["id"].as_u64()==Some(self.sequence),"unexpected QMP response");
            if let Some(error)=reply.get("error") {bail!("native engine: {error}");}
            // Complete LCD panels contain a large JSON string. Move it out
            // of the envelope instead of duplicating it on every update.
            return reply.as_object_mut().and_then(|value|value.remove("return"))
                .context("QMP reply lacks a result");
        }
        })();
        let property=request["arguments"]["property"].as_str();
        self.diagnostics.record_qmp(command,property,started.elapsed(),self.received_bytes.saturating_sub(received) as usize);
        if let Err(error)=&result {
            self.diagnostics.log("error","qmp",format!("{command} {} failed: {error:#}",property.unwrap_or("")));
        }
        result
    }
    pub fn property(&mut self,path:&str,name:&str) -> Result<Value> {
        let reply=self.execute("qom-get",json!({"path":path,"property":name}))?;
        let text=reply.as_str().context("QMP property is not JSON text")?;
        let decode_start=Instant::now();
        let value=serde_json::from_str::<Value>(text);
        self.diagnostics.record_qmp_decode(decode_start.elapsed());
        Ok(value?)
    }
    fn execute_with_timeout(&mut self,command:&str,arguments:Value,timeout:Duration) -> Result<Value> {
        self.reader.get_mut().set_read_timeout(Some(timeout))?;
        let result=self.execute(command,arguments);
        let reset=self.reader.get_mut().set_read_timeout(Some(Duration::from_secs(5)));
        match result {Ok(value)=>{reset?;Ok(value)},Err(error)=>Err(error)}
    }
}

pub struct Session {
    pub qmp:Qmp,
    pub audio:AudioBuffer,
    pub diagnostics:DiagnosticsHandle,
    pub files:PhoneFiles,
    _audio_transport:AudioTransport,
    _serial_transport:SerialTransport,
    display_transport:DisplayTransport,
    display_stream:bool,
    last_stream_revision:Option<u64>,
    process:Child,
    workspace:PathBuf,
    prepared:PathBuf,
    last_panel:Option<Value>,
    log_offset:u64,
    _boot_flash:Option<FlashExport>,
    card_inserted:bool,
    _lease:fs::File,
}

#[derive(Deserialize,Serialize)]
struct SavedCheckpoint {
    format_version:u32, file:String, checkpoint_tcm:Value,
    #[serde(default, skip_serializing_if="Option::is_none")]
    flash:Option<flash::FlashImages>,
    #[serde(default, skip_serializing_if="Option::is_none")]
    memory_generation:Option<String>,
}

pub(crate) fn atomic_replace(from:&Path,to:&Path) -> std::io::Result<()> {
    #[cfg(not(windows))] {fs::rename(from,to)}
    #[cfg(windows)] {
        use std::os::windows::ffi::OsStrExt;
        #[link(name="kernel32")]
        unsafe extern "system" {fn MoveFileExW(from:*const u16,to:*const u16,flags:u32)->i32;}
        let from:Vec<u16>=from.as_os_str().encode_wide().chain(Some(0)).collect();
        let to:Vec<u16>=to.as_os_str().encode_wide().chain(Some(0)).collect();
        if unsafe {MoveFileExW(from.as_ptr(),to.as_ptr(),1|8)}==0 {
            Err(std::io::Error::last_os_error())
        } else {Ok(())}
    }
}

pub(crate) fn sync_directory(path:&Path) -> std::io::Result<()> {
    #[cfg(unix)] {fs::File::open(path)?.sync_all()}
    // MoveFileExW above uses MOVEFILE_WRITE_THROUGH on Windows.
    #[cfg(not(unix))] {let _=path;Ok(())}
}

fn saved_name(name:&str) -> bool {
    name.starts_with("phone-") && name.ends_with(".vmstate") && !name.contains('/') && !name.contains('\\') && !name.contains("..")
}

fn parameter(path:&Path) -> String {path.to_string_lossy().replace(',',",,")}

fn engine_log_tail(path:&Path) -> String {
    let Ok(mut file)=fs::File::open(path) else {return String::new()};
    let Ok(length)=file.metadata().map(|metadata|metadata.len()) else {return String::new()};
    if file.seek(SeekFrom::Start(length.saturating_sub(16*1024))).is_err() {return String::new();}
    let mut bytes=Vec::with_capacity(length.min(16*1024) as usize);
    if file.take(16*1024).read_to_end(&mut bytes).is_err() {return String::new();}
    String::from_utf8_lossy(&bytes).into_owned()
}

const fn hex_table() -> [u8;256] {
    let mut table=[u8::MAX;256];
    let mut value=0;
    while value<16 {
        table[if value<10 {b'0' as usize+value} else {b'a' as usize+value-10}]=value as u8;
        if value>=10 {table[b'A' as usize+value-10]=value as u8;}
        value+=1;
    }
    table
}
const HEX_NIBBLES:[u8;256]=hex_table();

fn decode_lcd_hex(hex:&str) -> Result<Vec<u8>> {
    ensure!(hex.len()==176*220*4,"LCD dimensions differ");
    let mut pixels=Vec::with_capacity(176*220*2);
    for pair in hex.as_bytes().chunks_exact(2) {
        let hi=HEX_NIBBLES[pair[0] as usize];
        let lo=HEX_NIBBLES[pair[1] as usize];
        ensure!(hi|lo<16,"LCD hex");
        pixels.push((hi<<4)|lo);
    }
    Ok(pixels)
}

impl Session {
    pub fn start(options:&HostOptions,prepared:&Path) -> Result<Self> {
        Self::start_with_lease(options,prepared,None,None,AudioBuffer::default(),DiagnosticsHandle::default(),PhoneFiles::default())
    }
    pub fn start_prepared(options:&HostOptions,prepared:crate::prepare::Prepared) -> Result<Self> {
        Self::start_prepared_with_audio(options,prepared,AudioBuffer::default())
    }
    pub fn start_prepared_with_audio(options:&HostOptions,prepared:crate::prepare::Prepared,audio:AudioBuffer) -> Result<Self> {
        Self::start_prepared_with_diagnostics(options,prepared,audio,DiagnosticsHandle::default())
    }
    pub fn start_prepared_with_diagnostics(options:&HostOptions,prepared:crate::prepare::Prepared,audio:AudioBuffer,diagnostics:DiagnosticsHandle) -> Result<Self> {
        Self::start_prepared_with_services_and_diagnostics(options,prepared,audio,PhoneFiles::default(),diagnostics)
    }
    pub fn start_prepared_with_services(options:&HostOptions,prepared:crate::prepare::Prepared,audio:AudioBuffer,files:PhoneFiles) -> Result<Self> {
        Self::start_prepared_with_services_and_diagnostics(options,prepared,audio,files,DiagnosticsHandle::default())
    }
    pub fn start_prepared_with_services_and_diagnostics(options:&HostOptions,prepared:crate::prepare::Prepared,audio:AudioBuffer,files:PhoneFiles,diagnostics:DiagnosticsHandle) -> Result<Self> {
        Self::start_prepared_with_card(options,prepared,audio,files,diagnostics,None)
    }
    pub fn start_prepared_with_card(options:&HostOptions,prepared:crate::prepare::Prepared,audio:AudioBuffer,files:PhoneFiles,diagnostics:DiagnosticsHandle,card:Option<&Path>) -> Result<Self> {
        Self::start_with_lease(options,&prepared.state,Some(prepared.lease),card,audio,diagnostics,files)
    }
    pub fn open_prepared(engine:&Path,prepared:crate::prepare::Prepared,checkpoint:&Path) -> Result<Self> {
        Self::open_with_lease(engine,&prepared.state,Some(checkpoint),None,prepared.lease,AudioBuffer::default(),DiagnosticsHandle::default(),PhoneFiles::default(),None)
    }
    fn start_with_lease(options:&HostOptions,prepared:&Path,lease:Option<fs::File>,card:Option<&Path>,audio:AudioBuffer,diagnostics:DiagnosticsHandle,files:PhoneFiles) -> Result<Self> {
        let workspace=prepared.parent().context("prepared state directory")?;
        let lease=match lease {Some(lease)=>lease,None=>Self::lock(workspace)?};
        let mut checkpoint=None;
        let mut state:Value=serde_json::from_slice(&fs::read(prepared)?)?;
        if options.resume && card.is_none() {
            let metadata=workspace.join("phone.saved.json");
            if metadata.is_file() {
                let saved=read_checkpoint(&metadata)?;
                let memory=workspace.join(MEMORY_FILE);
                let matches=if memory.exists() {
                    let generation=flash::memory_generation(&memory)?.to_string();
                    saved.memory_generation.as_deref()==Some(generation.as_str())
                } else {saved.memory_generation.is_none()};
                let file=workspace.join(&saved.file);
                if matches && file.is_file() {
                    checkpoint=Some(file);
                    state["checkpoint_tcm"]=saved.checkpoint_tcm;
                }
            } else {
                let legacy=workspace.join("phone.vmstate");
                checkpoint=(!workspace.join(MEMORY_FILE).exists() && legacy.is_file()).then_some(legacy);
            }
        } else {state.as_object_mut().context("prepared state object")?.remove("checkpoint_tcm");}
        let temporary=workspace.join("state.json.pending");
        fs::write(&temporary,serde_json::to_vec_pretty(&state)?)?;
        atomic_replace(&temporary,prepared)?;
        Self::open_with_lease(&options.engine,prepared,checkpoint.as_deref(),card,lease,audio,diagnostics,files,options.midi_soundfont.as_deref())
    }
    fn lock(workspace:&Path) -> Result<fs::File> {
        let file=fs::OpenOptions::new().create(true).truncate(false).read(true).write(true)
            .open(workspace.join("session.lock"))?;
        file.lock().context("Waiting for the previous native phone session")?;
        Ok(file)
    }
    pub fn open(engine:&Path,prepared:&Path,checkpoint:Option<&Path>) -> Result<Self> {
        let lease=Self::lock(prepared.parent().context("prepared state directory")?)?;
        Self::open_with_lease(engine,prepared,checkpoint,None,lease,AudioBuffer::default(),DiagnosticsHandle::default(),PhoneFiles::default(),None)
    }
    fn open_with_lease(engine:&Path,prepared:&Path,checkpoint:Option<&Path>,card:Option<&Path>,lease:fs::File,audio:AudioBuffer,diagnostics:DiagnosticsHandle,files:PhoneFiles,midi_soundfont:Option<&Path>) -> Result<Self> {
        ensure!(engine.is_file(),"Native DB2010 engine is missing: {}",engine.display());
        let workspace=prepared.parent().context("prepared state directory")?.to_owned();
        // Instruction-count timing is useful for research, but throttles the
        // native Java software fallback and interrupts TCG for every 16-byte
        // LCD burst. Run native apps on QEMU's normal host-paced virtual clock.
        let mut live_state:Value=serde_json::from_slice(&fs::read(prepared)?)?;
        // Restore storage only for a cold boot. A VM checkpoint already owns
        // its NOR/NAND bytes, controller state and validity bookkeeping.
        let saved_metadata=workspace.join("phone.saved.json");
        let mut boot_flash=None;
        if checkpoint.is_none() {
            live_state.as_object_mut().context("prepared state object")?.remove("checkpoint_tcm");
            let memory=workspace.join(MEMORY_FILE);
            if memory.exists() {
                let flash=flash::unpack_memory(&workspace,&memory)?;
                flash::apply_directory(flash.path(),&mut live_state)?;
                diagnostics.log("info","storage","Cold boot with saved K750i internal memory.");
                boot_flash=Some(flash);
            } else if saved_metadata.exists() {
                if let Some(flash)=read_checkpoint(&saved_metadata)?.flash {flash.apply(&workspace,&mut live_state)?;}
            }
        }
        live_state["timebase"]=json!({"profile":"qemu-realtime-v1"});
        let launch_state=workspace.join("live-state.json");
        fs::write(&launch_state,serde_json::to_vec(&live_state)?)?;
        let listener=TcpListener::bind(("127.0.0.1",0))?;
        let port=listener.local_addr()?.port(); drop(listener);
        let log=fs::File::create(workspace.join("engine.log"))?;
        audio.set_diagnostics(diagnostics.clone());
        let audio_transport=AudioTransport::bind(audio.clone())?;
        let display_transport=DisplayTransport::bind(diagnostics.clone())?;
        let serial_transport=SerialTransport::bind_in(files.clone(),&workspace)?;
        let mut command=Command::new(engine);
        if let Some(path)=midi_soundfont {command.env("PH0NIX_MIDI_SOUNDFONT",path);}
        command.args(["-machine",&format!("db2010,research-guards=off,state={}",parameter(&launch_state)),
            "-accel","tcg,thread=single",
            "-nodefaults","-display","none","-monitor","none","-serial","none","-S",
            // The client setting covers requests only. Disable Nagle on
            // engine replies too, including the final short LCD segment.
            "-qmp",&format!("tcp:127.0.0.1:{port},server=on,wait=off,nodelay=on")]);
        command.args(["-chardev",&serial_transport.chardev()]);
        for port in 1..4 {command.args(["-chardev",&format!("file,id=uart{port},path={}",parameter(&workspace.join(format!("uart{port}.bin"))))]);}
        command.args(["-chardev",&audio_transport.chardev()]);
        command.args(["-chardev",&display_transport.chardev()]);
        if checkpoint.is_some() {command.args(["-incoming","defer"]);}
        let mut process=command.stdout(Stdio::null()).stderr(log).spawn().context("Starting native DB2010 engine")?;
        diagnostics.set_engine_pid(Some(process.id()));
        diagnostics.log("info","engine",format!("Native DB2010 started (PID {}, {} checkpoint).",process.id(),if checkpoint.is_some() {"restored"} else {"boot"}));
        let qmp=(|| -> Result<Qmp> {
        let deadline=Instant::now()+Duration::from_secs(15);
        loop {
            if let Some(status)=process.try_wait()? {
                bail!("Native engine exited ({status}): {}",engine_log_tail(&workspace.join("engine.log")));
            }
            match Qmp::connect_with_diagnostics(port,diagnostics.clone()) {
                Ok(client)=>return Ok(client),
                Err(error)=>{
                    if Instant::now()>=deadline {return Err(error.context("Native engine startup deadline"));}
                    thread::sleep(Duration::from_millis(10));
                }
            }
        }
        })();
        // Failures before Session construction cannot use its Drop cleanup.
        // Reap the child and detach its PID from the retained developer panel.
        let qmp=match qmp {
            Ok(qmp)=>qmp,
            Err(error)=>{
                let _=process.kill(); let _=process.wait();
                diagnostics.set_running(false);
                diagnostics.set_engine_pid(None);
                return Err(error);
            }
        };
        let mut session=Self {qmp,audio,diagnostics,files,_audio_transport:audio_transport,_serial_transport:serial_transport,
            display_transport,display_stream:false,last_stream_revision:None,
            process,workspace,prepared:prepared.to_owned(),last_panel:None,log_offset:0,_boot_flash:boot_flash,card_inserted:false,_lease:lease};
        // Engines without binary transport retain the cached QMP path. Negotiate once;
        // current engines send complete binary panels without per-frame RPCs.
        let properties=session.qmp.execute("qom-list",json!({"path":"/machine"}))?;
        if properties.as_array().is_some_and(|properties|properties.iter().any(|p|p["name"]=="display-stream")) {
            session.display_stream=session.qmp.execute("qom-get",json!({"path":"/machine","property":"display-stream"}))?
                .as_bool().unwrap_or(false);
        }
        // Require the complete-panel native transport, without waking the CPU.
        session.qmp.property("/machine","display-status")?;
        if let Some(checkpoint)=checkpoint {session.migrate("migrate-incoming",checkpoint)?;}
        if let Some(card)=card {session.attach_memory_stick(card)?;}
        session.qmp.execute("cont",json!({}))?;
        Ok(session)
    }
    pub fn frame(&mut self) -> Result<Option<LcdFrame>> {
        let started=Instant::now();
        let frame=self.read_frame();
        self.diagnostics.record_poll(started.elapsed(),frame.as_ref().is_ok_and(|frame|frame.is_some()));
        frame
    }
    fn read_frame(&mut self) -> Result<Option<LcdFrame>> {
        if self.display_stream {
            let Some(frame)=self.display_transport.take()? else {return Ok(None)};
            if self.last_stream_revision==Some(frame.revision) {return Ok(None);}
            self.last_stream_revision=Some(frame.revision);
            return Ok(Some(frame));
        }
        let status=self.qmp.property("/machine","display-status")?;
        if self.last_panel.as_ref()==Some(&status) {return Ok(None);}
        self.last_panel=Some(status.clone());
        if status["known_pixels"].as_u64()!=Some(176*220) {return Ok(None);}
        let panel=self.qmp.property("/machine","display-cached")?;
        let decode_start=Instant::now();
        let hex=panel["pixels_hex"].as_str().context("LCD pixels are missing")?;
        let pixels=decode_lcd_hex(hex)?;
        let frame=LcdFrame {width:176,height:220,rgb565_be:pixels,
            revision:panel["frames"].as_u64().context("LCD revision")?};
        self.diagnostics.record_frame_decode(decode_start.elapsed());
        Ok(Some(frame))
    }
    fn attach_memory_stick(&mut self,path:&Path) -> Result<()> {
        let card=CardImage::open(path)?;
        let absolute=card.path().canonicalize()?;
        let name=absolute.to_str().context("Memory Stick image path is not UTF-8")?;
        self.qmp.execute("qom-set",json!({"path":"/machine","property":"memorystick-image","value":name}))?;
        self.card_inserted=true;
        self.diagnostics.log("info","storage",format!("Booting with Memory Stick: {}",absolute.display()));
        Ok(())
    }
    pub fn will_save_checkpoint(&self,resume:bool) -> bool {
        resume && !self.card_inserted
    }
    pub fn contacts(&mut self,keys:&[&str]) -> Result<()> {
        self.qmp.execute("qom-set",json!({"path":"/machine/keypad","property":"pressed",
            "value":serde_json::to_string(keys)?}))?;
        self.diagnostics.log("info","keypad",if keys.is_empty() {"All contacts released".to_owned()} else {format!("Pressed: {}",keys.join(" + "))});
        Ok(())
    }
    pub fn progress(&mut self) -> Result<Value> {
        let progress=self.qmp.property("/machine","progress")?;
        self.diagnostics.update_guest(progress["virtual_ns"].as_u64(),progress["observations"].as_u64(),
            progress["pc"].as_u64().and_then(|pc|u32::try_from(pc).ok()),progress["running"].as_bool().unwrap_or(false));
        self.diagnostics.update_engine_components(&progress["host_profile"]);
        self.read_engine_log();
        Ok(progress)
    }
    pub fn snapshot_chunks(&mut self) -> Result<Vec<Value>> {
        // Full firmware snapshots can exceed the QMP frame bound. Keep each
        // authenticated engine chunk bounded and save its original envelope.
        self.qmp.execute("qom-set", serde_json::json!({"path":"/machine",
            "property":"snapshot-chunk","value":"start"}))?;
        let mut chunks = Vec::new();
        let mut offset = 0;
        loop {
            let chunk = self.qmp.property("/machine", "snapshot-chunk")?;
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
        Ok(chunks)
    }
    pub fn summary(&mut self) -> Result<Value> {
        let summary=self.qmp.property("/machine","summary")?;
        if !summary["stop"].is_null() {
            self.diagnostics.log("error","firmware",format!("Firmware stop: {}",summary["stop"]));
        }
        self.read_engine_log();
        Ok(summary)
    }
    // Read only new bounded stderr bytes. Diagnostic polling never loads an
    // ever-growing engine.log or a stopped-only firmware summary.
    fn read_engine_log(&mut self) {
        let Ok(mut file)=fs::File::open(self.workspace.join("engine.log")) else {return};
        let Ok(length)=file.metadata().map(|metadata|metadata.len()) else {return};
        if length<=self.log_offset {return;}
        let offset=self.log_offset.max(length.saturating_sub(16*1024));
        if file.seek(SeekFrom::Start(offset)).is_err() {return;}
        let mut bytes=Vec::with_capacity((length-offset).min(16*1024) as usize);
        if file.take(16*1024).read_to_end(&mut bytes).is_err() {return;}
        self.log_offset=offset+bytes.len() as u64;
        for line in String::from_utf8_lossy(&bytes).lines() {
            let level=if line.to_ascii_lowercase().contains("error") {"error"} else {"info"};
            self.diagnostics.log(level,"engine-stderr",line);
        }
    }
    pub fn running(&mut self) -> Result<bool> {
        let status=self.qmp.execute("query-status",json!({}))?;
        Ok(status["running"].as_bool().unwrap_or(false))
    }
    fn migrate(&mut self,operation:&str,path:&Path) -> Result<()> {
        self.qmp.execute(operation,json!({"channels":[{"channel-type":"main","addr":{
            "transport":"file","filename":path,"offset":0}}]}))?;
        let deadline=Instant::now()+Duration::from_secs(20);
        loop {
            let status=self.qmp.execute("query-migrate",json!({}))?;
            match status["status"].as_str() {
                Some("completed")=>return Ok(()),
                Some("failed"|"cancelled")=>bail!("Native checkpoint failed: {status}"),
                _=>ensure!(Instant::now()<deadline,"Native checkpoint deadline"),
            }
            thread::sleep(Duration::from_millis(10));
        }
    }
    pub fn pause(&mut self) -> Result<()> {
        self.contacts(&[])?;
        self.qmp.execute("stop", json!({}))?;
        ensure!(self.summary()?["stop"].is_null(), "Firmware stopped with an error before pausing");
        self.diagnostics.set_running(false);
        Ok(())
    }

    pub fn resume(&mut self) -> Result<()> {
        self.audio.discard_pending();
        self.qmp.execute("cont", json!({}))?;
        self.diagnostics.set_running(true);
        Ok(())
    }

    pub fn save(&mut self) -> Result<()> { self.save_with_resume(true) }

    pub fn save_with_resume(&mut self, resume:bool) -> Result<()> {
        let start=Instant::now();
        let checkpoint=self.will_save_checkpoint(resume);
        let result=self.save_phone(resume).and_then(|()|self.files.commit_saved());
        self.diagnostics.record_save_phase(&format!("save_total card={} resume={resume} checkpoint={checkpoint}",
            self.card_inserted),start.elapsed(),result.is_ok());
        if let Err(error)=&result {
            self.files.fail_pending_commit(&format!("{error:#}"));
            self.diagnostics.log("error","storage",format!("Phone save failed: {error:#}"));
        }
        result
    }
    /// A live file move checkpoints the phone without closing its session.
    /// Do not resume an actual firmware fault after a failed save.
    pub fn resume_after_save(&mut self) -> Result<()> {
        ensure!(self.summary()?["stop"].is_null(),"Firmware stopped with an error during file save");
        self.qmp.execute("cont",json!({}))?;
        self.files.resume_after_save();
        self.last_panel=None;
        Ok(())
    }
    /// Save only committed flash after a rejected PMIC write. The faulted VM
    /// and a potentially interrupted file transfer must never be resumed.
    pub fn recover_internal_memory_after_pmic_stop(&mut self) -> Result<bool> {
        let status=self.qmp.execute("query-status",json!({}))?;
        if status["status"]!="debug" { return Ok(false); }
        let summary=self.summary()?;
        let stop=&summary["stop"];
        if stop["reason"]!="i2c_extra_write_value_unmodeled" ||
            !matches!(stop["address"].as_u64(),Some(0xf9040000|0xf9040002)) {
            return Ok(false);
        }
        ensure!(!self.files.snapshot().busy,
            "A phone file transfer was interrupted; the previous saved memory was kept");
        self.pack_internal_memory()?.publish(&self.workspace)?;
        remove_old_checkpoint(&self.workspace);
        self.diagnostics.log("info","storage","Internal memory saved after the PMIC stop; the next launch will cold boot.");
        Ok(true)
    }
    fn allow_card_writeback(&mut self) -> Result<()> {
        // R1DB001 can acknowledge an OBEX PUT while file data and FAT
        // metadata are still cached. Keep firmware running after DISCONNECT.
        const GUEST_NS: u64 = 30_000_000_000;
        let mut start = None;
        let deadline = Instant::now() + Duration::from_secs(90);
        self.diagnostics.log("info", "storage", "Waiting for Memory Stick writes to finish.");
        loop {
            let progress = self.progress()?;
            ensure!(progress["running"] == true, "Firmware stopped while saving the Memory Stick: {}",
                self.summary()?["stop"]);
            let now = progress["virtual_ns"].as_u64().context("Missing firmware clock")?;
            if now.saturating_sub(*start.get_or_insert(now)) >= GUEST_NS { return Ok(()); }
            ensure!(Instant::now() < deadline, "Firmware did not run long enough to finish saving the Memory Stick");
            thread::sleep(Duration::from_millis(250));
        }
    }

    fn save_phone(&mut self, resume:bool) -> Result<()> {
        // An OBEX session is host state, not checkpoint state. Finish its
        // queued operation and return firmware to AT mode while the VM can
        // still acknowledge the disconnect; ambiguous streams cannot save.
        let diagnostics=self.diagnostics.clone();
        diagnostics.measure_save("files_quiesce", || self.files.quiesce(Duration::from_secs(20)))
            .context("Disconnecting phone file service before checkpoint")?;
        self.contacts(&[])?;
        if self.card_inserted {
            diagnostics.measure_save("card_writeback", || self.allow_card_writeback())?;
        }
        self.qmp.execute("stop",json!({}))?;
        ensure!(self.summary()?["stop"].is_null(),
            "Firmware stopped with an error; the previous saved phone state was kept");
        let memory=self.pack_internal_memory()?;
        if !self.will_save_checkpoint(resume) {
            diagnostics.measure_save("memory_publish", || memory.publish(&self.workspace))?;
            remove_old_checkpoint(&self.workspace);
            return Ok(());
        }
        let layout=self.qmp.property("/machine","checkpoint-layout")?;
        let mut state:Value=serde_json::from_slice(&fs::read(&self.prepared)?)?;
        state["timebase"]=json!({"profile":"qemu-realtime-v1"});
        state["checkpoint_tcm"]=layout;
        let nonce=std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_nanos();
        let name=format!("phone-{nonce}-{}.vmstate",std::process::id());
        let temporary=self.workspace.join(format!("{name}.pending"));
        let final_path=self.workspace.join(&name);
        let metadata=self.workspace.join("phone.saved.json");
        let previous=read_checkpoint(&metadata).ok();
        let pending=self.workspace.join("phone.saved.json.pending");
        let result=(|| -> Result<()> {
            diagnostics.measure_save("vm_migrate", || self.migrate("migrate",&temporary))?;
            diagnostics.measure_save("vm_sync", || -> Result<()> {
                atomic_replace(&temporary,&final_path)?;
                fs::OpenOptions::new().write(true).open(&final_path)?.sync_all()?;
                fs::write(&pending,serde_json::to_vec(&SavedCheckpoint {
                    format_version:1,file:name.clone(),checkpoint_tcm:state["checkpoint_tcm"].clone(),
                    flash:None,memory_generation:Some(memory.generation().to_string()),
                })?)?;
                fs::OpenOptions::new().write(true).open(&pending)?.sync_all()?;
                Ok(())
            })?;
            diagnostics.measure_save("memory_publish", || memory.publish(&self.workspace))?;
            atomic_replace(&pending,&metadata)?;
            Ok(())
        })();
        if result.is_err() {
            let _=fs::remove_file(&temporary);
            let _=fs::remove_file(&final_path);
            let _=fs::remove_file(&pending);
        }
        result?;
        diagnostics.measure_save("vm_directory_sync", || sync_directory(&self.workspace))
            .context("Synchronizing phone checkpoint directory")?;
        if let Some(previous)=previous { remove_checkpoint_files(&self.workspace,previous); }
        Ok(())
    }
    fn pack_internal_memory(&mut self) -> Result<PendingMemory> {
        let export=FlashExport::new(&self.workspace)?;
        // Raw images total almost 64 MiB. Allow slower phone storage more
        // time than the normal five-second observation deadline.
        self.diagnostics.measure_save("flash_export", || self.qmp.execute_with_timeout("qom-set",
            json!({"path":"/machine","property":"flash-byte-export","value":export.path()}),
            Duration::from_secs(30))).context("Saving internal K750 storage")?;
        PendingMemory::pack(&self.workspace,export.path(),Some(&self.diagnostics))
    }
}
fn read_checkpoint(metadata:&Path) -> Result<SavedCheckpoint> {
    let saved:SavedCheckpoint=serde_json::from_slice(&fs::read(metadata)?)?;
    ensure!(saved.format_version==1 && saved_name(&saved.file),"Invalid native saved state metadata");
    Ok(saved)
}

fn remove_checkpoint_files(workspace:&Path, saved:SavedCheckpoint) {
    let _=fs::remove_file(workspace.join(saved.file));
    if let Some(flash)=saved.flash { flash.remove(workspace); }
}

pub(crate) fn remove_old_checkpoint(workspace:&Path) {
    let metadata=workspace.join("phone.saved.json");
    if let Ok(saved)=read_checkpoint(&metadata) {
        if fs::remove_file(metadata).is_ok() { remove_checkpoint_files(workspace,saved); }
    }
}

/// Export from a phone workspace under its normal session lock.
pub fn export_internal_memory(workspace:&Path, destination:&Path) -> Result<()> {
    if !workspace.is_dir() { bail!("No saved K750i internal memory. Run the phone and return to Library first."); }
    let _lease=Session::lock(workspace)?;
    let memory=workspace.join(MEMORY_FILE);
    if memory.exists() { return flash::export_memory(&memory,destination); }
    let metadata=workspace.join("phone.saved.json");
    if metadata.is_file() {
        if let Some(flash)=read_checkpoint(&metadata)?.flash { return flash.export(workspace,destination); }
    }
    bail!("No saved K750i internal memory. Run the phone and return to Library first.")
}

/// Verify the complete backup before replacing flash. An old VM checkpoint
/// cannot resume against the new generation, including after an interrupted import.
pub fn import_internal_memory(workspace:&Path, source:&Path) -> Result<()> {
    fs::create_dir_all(workspace)?;
    let _lease=Session::lock(workspace)?;
    PendingMemory::import(workspace,source)?.publish(workspace)?;
    remove_old_checkpoint(workspace);
    Ok(())
}

impl Drop for Session {
    fn drop(&mut self) {
        self.read_engine_log();
        let _=self.qmp.execute("quit",json!({}));
        let deadline=Instant::now()+Duration::from_secs(2);
        while Instant::now()<deadline {
            if self.process.try_wait().ok().flatten().is_some() {self.diagnostics.set_running(false);self.diagnostics.set_engine_pid(None);self.read_engine_log();return;}
            thread::sleep(Duration::from_millis(10));
        }
        let _=self.process.kill(); let _=self.process.wait();
        self.diagnostics.set_running(false);
        self.diagnostics.set_engine_pid(None);
        self.read_engine_log();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires the built DB2010 engine and a generated PMIC fixture"]
    fn saves_committed_flash_after_pmic_fault() -> Result<()> {
        let engine = std::env::var("PH0NIX_TEST_ENGINE")?;
        let state = std::env::var("PH0NIX_PMIC_FIXTURE")?;
        let state = Path::new(&state);
        let workspace = state.parent().context("fixture directory")?;
        let mut session = Session::open(Path::new(&engine), state, None)?;
        let deadline = Instant::now() + Duration::from_secs(10);
        while session.running()? {
            ensure!(Instant::now() < deadline, "PMIC fixture did not stop");
            thread::sleep(Duration::from_millis(10));
        }
        let stop = session.summary()?["stop"].clone();
        assert_eq!(stop["reason"], "i2c_extra_write_value_unmodeled", "{stop}");
        assert_eq!(stop["address"], 0xf9040000u32, "{stop}");
        let log = fs::read_to_string(workspace.join("engine.log"))?;
        assert!(log.contains("PMIC register 0xa0, value 0x2e"), "{log}");
        assert!(session.save_with_resume(false).is_err());
        assert!(session.recover_internal_memory_after_pmic_stop()?);
        let export = flash::unpack_memory(workspace, &workspace.join(MEMORY_FILE))?;
        let gdfs = fs::read(export.path().join("gdfs.bin"))?;
        assert_eq!(&gdfs[..4], &[0x34, 0x12, 0xff, 0xff]);
        Ok(())
    }

    #[test]
    fn legacy_saved_checkpoint_without_flash_still_loads() {
        let bytes=br#"{"format_version":1,"file":"phone-123-456.vmstate","checkpoint_tcm":{}}"#;
        let saved:SavedCheckpoint=serde_json::from_slice(bytes).unwrap();
        assert!(saved.flash.is_none());
        assert!(saved_name(&saved.file));
        assert_eq!(serde_json::from_slice::<Value>(&serde_json::to_vec(&saved).unwrap()).unwrap(),
            serde_json::from_slice::<Value>(bytes).unwrap());
    }

    #[test]
    fn occasional_slow_panel_recovers_the_poll_cadence_and_long_stalls_are_bounded() {
        let start=Instant::now();
        let mut due=start;
        let (delay,missed)=poll_delay(&mut due,start+Duration::from_millis(23));
        assert_eq!(delay,Duration::ZERO);
        assert_eq!(missed,1);
        let (delay,missed)=poll_delay(&mut due,start+Duration::from_millis(29));
        assert!(delay>Duration::ZERO && delay<PRESENTATION_INTERVAL);
        assert_eq!(missed,0);
        let now=start+Duration::from_secs(20);
        let (delay,missed)=poll_delay(&mut due,now);
        assert_eq!(delay,Duration::ZERO);
        assert!(missed>1000);
        assert!(now.saturating_duration_since(due)<PRESENTATION_INTERVAL);
    }

    #[test]
    fn lcd_decoder_preserves_all_byte_values_and_rejects_invalid_panels() {
        let bytes:Vec<_>=(0..176*220*2).map(|i|(i%256) as u8).collect();
        let hex=bytes.iter().map(|byte|format!("{byte:02X}")).collect::<String>();
        assert_eq!(decode_lcd_hex(&hex).unwrap(),bytes);
        assert_eq!(decode_lcd_hex(&hex.to_lowercase()).unwrap(),bytes);
        assert!(decode_lcd_hex(&hex[..hex.len()-2]).is_err());
        for invalid in [b'g',b'/',b':',b' ',0xff] {
            let mut bad=hex.clone().into_bytes(); bad[123]=invalid;
            if let Ok(text)=String::from_utf8(bad) {assert!(decode_lcd_hex(&text).is_err());}
        }
    }

    #[test]
    fn loopback_requests_preserve_ids_and_handle_interleaved_events() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            let mut peer = BufReader::new(listener.accept().unwrap().0);
            peer.get_mut().write_all(b"{\"QMP\":{}}\n").unwrap();
            for (index, expected) in ["qmp_capabilities", "qom-get", "stop", "cont"].iter().enumerate() {
                let mut line = String::new();
                peer.read_line(&mut line).unwrap();
                let request: Value = serde_json::from_str(&line).unwrap();
                assert_eq!(request["execute"], *expected);
                assert_eq!(request["id"], index + 1);
                let reply = match *expected {
                    "qom-get" => json!({"return": "{\"frames\":7}", "id": index + 1}),
                    "stop" => {
                        peer.get_mut().write_all(b"{\"event\":\"STOP\"}\n").unwrap();
                        json!({"return": {}, "id": index + 1})
                    }
                    "cont" => {
                        peer.get_mut().write_all(b"{\"event\":\"RESUME\"}\n").unwrap();
                        json!({"return": {}, "id": index + 1})
                    }
                    _ => json!({"return": {}, "id": index + 1}),
                };
                let mut bytes = serde_json::to_vec(&reply).unwrap();
                bytes.push(b'\n');
                peer.get_mut().write_all(&bytes).unwrap();
            }
        });
        let mut qmp = Qmp::connect(port).unwrap();
        assert_eq!(qmp.property("/machine", "display-status").unwrap()["frames"], 7);
        qmp.execute("stop", json!({})).unwrap();
        assert!(qmp.stopped);
        qmp.execute("cont", json!({})).unwrap();
        assert!(!qmp.stopped);
        server.join().unwrap();
    }
}
