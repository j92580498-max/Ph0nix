// Derived from tayga16/SamsungE380-Emu; original Apache-2.0/MIT terms retained.
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{atomic::{AtomicBool, AtomicU32, Ordering}, mpsc, Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use e830::devices::{runtime::{Frame, RunReport, RuntimeOptions}};
use e830::StopReason;
pub use db2010_host::{HostOptions, LcdFrame};
pub use db2010_host::audio::{AudioBuffer, PlaybackPacket};
pub use db2010_host::diagnostics::{DiagnosticLog, DiagnosticsHandle, DiagnosticsSnapshot, TimingSnapshot};
pub use db2010_host::storage::PhoneFiles;
use db2010_host::qmp::{Session, PRESENTATION_INTERVAL, poll_delay};
pub use e830::devices::selection::FirmwareSelection;
pub use e830::devices::e830::validate_nand_image;
pub use e830::firmware::e830::{FirmwarePaths, classify_firmware_files};

// Preserve the existing worker pacing and one contact transition per quantum.
const NORMAL_RUN_QUANTUM: u64 = 2_000_000;
const INPUT_RUN_QUANTUM: u64 = 1_000_000;

pub enum WorkerEvent {
    Frame(Frame),
    Lcd(LcdFrame),
    Status(String),
    InputApplied(String),
    Finished(Result<String, String>),
}

const MAX_PENDING_EVENTS: usize = 64;
struct PendingEvents { queue: VecDeque<WorkerEvent>, receiver_alive: bool, sender_alive: bool }
/// A stalled UI retains one latest LCD panel, rather than an unlimited series
/// of obsolete complete panels. Control events preserve their relative order.
pub struct WorkerEvents { inner: Arc<Mutex<PendingEvents>> }
struct EventSender { inner: Arc<Mutex<PendingEvents>> }
fn event_channel() -> (EventSender, WorkerEvents) {
    let inner = Arc::new(Mutex::new(PendingEvents {
        queue: VecDeque::with_capacity(MAX_PENDING_EVENTS), receiver_alive: true, sender_alive: true,
    }));
    (EventSender { inner: inner.clone() }, WorkerEvents { inner })
}
impl EventSender {
    fn send(&self, event: WorkerEvent) -> Result<(), mpsc::SendError<WorkerEvent>> {
        let mut pending = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        if !pending.receiver_alive { return Err(mpsc::SendError(event)); }
        match &event {
            WorkerEvent::Frame(_) | WorkerEvent::Lcd(_) =>
                pending.queue.retain(|event| !matches!(event, WorkerEvent::Frame(_) | WorkerEvent::Lcd(_))),
            WorkerEvent::Status(_) => pending.queue.retain(|event| !matches!(event, WorkerEvent::Status(_))),
            _ => {},
        }
        if pending.queue.len() == MAX_PENDING_EVENTS {
            let oldest = pending.queue.iter().position(|event|
                matches!(event, WorkerEvent::Status(_) | WorkerEvent::InputApplied(_))).unwrap_or(0);
            pending.queue.remove(oldest);
        }
        pending.queue.push_back(event);
        Ok(())
    }
}
impl Drop for EventSender {
    fn drop(&mut self) { self.inner.lock().unwrap_or_else(|error| error.into_inner()).sender_alive = false; }
}
impl WorkerEvents {
    pub fn try_recv(&self) -> Result<WorkerEvent, mpsc::TryRecvError> {
        let mut pending = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        pending.queue.pop_front().ok_or(if pending.sender_alive { mpsc::TryRecvError::Empty } else { mpsc::TryRecvError::Disconnected })
    }
}
impl Drop for WorkerEvents {
    fn drop(&mut self) {
        let mut pending = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        pending.receiver_alive = false; pending.queue.clear();
    }
}

fn describe_key_transition(previous: u32, keys: u32) -> String {
    let mut changes = Vec::new();
    for key in e830::PhoneKey::ALL {
        if (previous ^ keys) & key.bit() != 0 {
            changes.push(format!("{key:?} {}", if keys & key.bit() != 0 { "down" } else { "up" }));
        }
    }
    if changes.is_empty() { "no contact change".into() } else { changes.join(", ") }
}

fn input_history_suffix(history: &VecDeque<String>) -> String {
    if history.is_empty() { String::new() } else {
        format!("; recent input: {}", history.iter().cloned().collect::<Vec<_>>().join(" | "))
    }
}

enum WorkerCommand {
    Keys(u32), SetPaused(bool), Stop,
}

pub struct Worker {
    input: mpsc::Sender<WorkerCommand>,
    last_input: AtomicU32,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
    pub events: WorkerEvents,
    pub audio: AudioBuffer,
    pub diagnostics: DiagnosticsHandle,
    pub files: PhoneFiles,
    card: Option<PathBuf>,
}

impl Worker {
    pub fn start(paths: impl Into<FirmwareSelection>) -> std::io::Result<Self> {
        Self::start_with_options(paths, None)
    }

    pub fn start_with_options(paths: impl Into<FirmwareSelection>, options: Option<HostOptions>) -> std::io::Result<Self> {
        Self::start_with_card(paths, options, None, None)
    }

    pub fn start_with_card(paths: impl Into<FirmwareSelection>, options: Option<HostOptions>,
        workspace: Option<PathBuf>, card: Option<PathBuf>,
    ) -> std::io::Result<Self> {
        let paths = paths.into();
        if card.is_some() && paths.device != "k750i" {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "Select the K750i to use a Memory Stick"));
        }
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let (sender, events) = event_channel();
        let (input, worker_input) = mpsc::channel();
        let audio = AudioBuffer::default();
        let worker_audio = audio.clone();
        let diagnostics = DiagnosticsHandle::default();
        audio.set_diagnostics(diagnostics.clone());
        let worker_diagnostics = diagnostics.clone();
        let files = PhoneFiles::default();
        let worker_files = files.clone();
        let worker_card = card.clone();
        let handle = std::thread::Builder::new().name("phone-firmware".into()).spawn(move || {
            // Unicorn owns Rc state and must be constructed inside this thread.
            let result = (|| -> anyhow::Result<String> {
                worker_diagnostics.log("info", "worker", format!("Starting {} firmware.", paths.device));
                if paths.device == "k750i" {
                    let options = match options {
                        Some(options) => options,
                        None => HostOptions::discover(&std::env::current_dir()?)?,
                    };
                    anyhow::ensure!(paths.files.companions.keys().all(|key| key == "fs"),
                        "The native K750 profile accepts its audited MAIN and FS images; use the research runner for physical ROM/GDFS overrides.");
                    let fs = paths.files.companions.get("fs").ok_or_else(|| anyhow::anyhow!("Select the matching FS .fbn image."))?;
                    let prepared = match workspace {
                        Some(directory) => db2010_host::prepare::prepare_locked_at(&paths.files.image, fs, &directory)?,
                        None => db2010_host::prepare::prepare_locked(&paths.files.image, fs, &options.data_dir)?,
                    };
                    let mut session = Session::start_prepared_with_card(&options, prepared,
                        worker_audio, worker_files.clone(), worker_diagnostics.clone(), worker_card.as_deref())?;
                    let result = (|| -> anyhow::Result<()> {
                        let mut due = Instant::now();
                        let mut status_due = due;
                        let mut input_due = due;
                        let mut pending_input = None;
                        let mut pause_requested = false;
                        let mut paused = false;
                        while !worker_stop.load(Ordering::Relaxed) {
                            match pending_input.take().or_else(|| worker_input.try_recv().ok()) {
                                Some(WorkerCommand::SetPaused(next)) => {
                                    pause_requested = next;
                                    if paused && !next {
                                        session.resume()?;
                                        paused = false;
                                        due = Instant::now();
                                        status_due = due;
                                        input_due = due;
                                    }
                                }
                                Some(WorkerCommand::Keys(keys)) if !paused => {
                                    if Instant::now() >= input_due {
                                        session.contacts(&crate::keys::qmp_contacts(keys))?;
                                        input_due = Instant::now() + PRESENTATION_INTERVAL;
                                    } else {
                                        pending_input = Some(WorkerCommand::Keys(keys));
                                    }
                                }
                                Some(WorkerCommand::Stop) => break,
                                _ => {}
                            }
                            // Let an accepted file transfer finish; suspending the guest
                            // midstream would time out the phone's live OBEX connection.
                            if pause_requested && !paused && !worker_files.snapshot().busy {
                                session.pause()?;
                                paused = true;
                            }
                            if paused {
                                // Wait for a command without polling the frozen guest.
                                pending_input = worker_input.recv().ok();
                                if pending_input.is_none() { break; }
                                continue;
                            }
                            if worker_files.pending_commit() {
                                worker_diagnostics.log("info", "checkpoint", "Saving imported file before completing its move.");
                                if let Err(error) = session.save_with_resume(options.resume) {
                                    worker_diagnostics.log("error", "files", format!("File move could not finish: {error:#}"));
                                }
                                session.resume_after_save()?;
                                // Checkpoint time is an intentional pause, not a
                                // rendering deadline miss or a backlog of polls.
                                due = Instant::now();
                                status_due = due;
                            }
                            if Instant::now() >= due {
                                if let Some(frame) = session.frame()? {
                                    if sender.send(WorkerEvent::Lcd(frame)).is_err() { break; }
                                }
                                let (_, missed) = poll_delay(&mut due, Instant::now());
                                if missed != 0 { worker_diagnostics.record_missed_deadlines(missed); }
                            }
                            if Instant::now() >= status_due {
                                let progress = session.progress()?;
                                let status = format!("Running Sony Ericsson K750i · {:.1} s",
                                    progress["virtual_ns"].as_u64().unwrap_or(0) as f64 / 1e9);
                                if sender.send(WorkerEvent::Status(status)).is_err() { break; }
                                if !progress["running"].as_bool().unwrap_or(false) {
                                    let summary = session.summary()?;
                                    let stop = &summary["stop"];
                                    anyhow::bail!("Firmware stopped: {} at 0x{:08X}, PC=0x{:08X}",
                                        stop["reason"].as_str().unwrap_or("engine paused"),
                                        stop["address"].as_u64().unwrap_or(0), stop["pc"].as_u64().unwrap_or(0));
                                }
                                status_due = Instant::now() + Duration::from_millis(250);
                            }
                            let mut next_due = due.min(status_due);
                            if pending_input.is_some() { next_due = next_due.min(input_due); }
                            let delay = next_due.saturating_duration_since(Instant::now());
                            if pending_input.is_some() {
                                std::thread::sleep(delay);
                            } else {
                                match worker_input.recv_timeout(delay) {
                                    Ok(command) => pending_input = Some(command),
                                    Err(mpsc::RecvTimeoutError::Timeout) => {},
                                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                                }
                            }
                        }
                        // A final save may need the running guest to disconnect its file service.
                        if paused { session.resume()?; }
                        session.save_with_resume(options.resume)
                    })();
                    if let Err(error) = result {
                        match session.recover_internal_memory_after_pmic_stop() {
                            Ok(true) => anyhow::bail!("{error:#}. Internal memory saved; the next launch will cold boot."),
                            Ok(false) => return Err(error),
                            Err(recovery) => anyhow::bail!("{error:#}. Could not recover internal memory: {recovery:#}"),
                        }
                    }
                    return Ok(if session.will_save_checkpoint(options.resume) { "Stopped. Phone session and internal storage saved." }
                        else { "Stopped. Internal storage saved." }.into());
                }
                let registry = crate::registry();
                let mut machine = registry.open(&paths.device, paths.loader.as_deref(), &paths.files, RuntimeOptions::default())?;
                let mut last_lcd_revision = 0;
                let mut latest_frame = None;
                let mut last_emit_at = Instant::now() - Duration::from_secs(1);
                let mut last_report: Option<RunReport> = None;
                let mut last_keys = 0;
                let mut input_history = VecDeque::with_capacity(8);
                let mut paused = false;
                while !worker_stop.load(Ordering::Relaxed) {
                    let command = if paused { worker_input.recv().ok() } else { worker_input.try_recv().ok() };
                    match command {
                        Some(WorkerCommand::SetPaused(next)) => {
                            if next && !paused {
                                machine.set_phone_keys(0)?;
                                last_keys = 0;
                            }
                            paused = next;
                            worker_diagnostics.set_running(!paused);
                            continue;
                        }
                        Some(WorkerCommand::Stop) => break,
                        _ if paused => continue,
                        _ => {}
                    }
                    // Apply at most one contact transition per guest quantum.
                    // This preserves ordering while ensuring a queued release
                    // cannot overtake the firmware's opportunity to service
                    // the preceding press interrupt.
                    let input_applied = if let Some(WorkerCommand::Keys(keys)) = command {
                        let instructions = last_report.as_ref().map_or(0, |report| report.execution.instructions);
                        let pc = last_report.as_ref().map_or(0, |report| report.execution.pc);
                        let entry = format!(
                            "{} at {} (mask=0x{keys:06x}, PC=0x{pc:08x})",
                            describe_key_transition(last_keys, keys), instructions,
                        );
                        machine.set_phone_keys(keys)?;
                        last_keys = keys;
                        if input_history.len() == 8 { input_history.pop_front(); }
                        input_history.push_back(entry.clone());
                        worker_diagnostics.log("info", "keypad", &entry);
                        if sender.send(WorkerEvent::InputApplied(entry)).is_err() {
                            return Ok("Window closed.".into());
                        }
                        true
                    } else { false };
                    let quantum = if input_applied { INPUT_RUN_QUANTUM } else { NORMAL_RUN_QUANTUM };
                    let cpu_start = Instant::now();
                    let report = machine.run(quantum).map_err(|error| anyhow::anyhow!(
                        "running guest: {error}{}", input_history_suffix(&input_history)
                    ))?;
                    worker_diagnostics.record_cpu_quantum(cpu_start.elapsed());
                    worker_diagnostics.update_guest(None, Some(report.execution.instructions), Some(report.execution.pc), true);
                    let mut emitted = false;
                    let revision = machine.frame_revision();
                    if revision != last_lcd_revision {
                        last_lcd_revision = revision;
                        let frame_start = Instant::now();
                        latest_frame = machine.frame();
                        worker_diagnostics.record_frame_decode(frame_start.elapsed());
                    }
                    if last_emit_at.elapsed() >= PRESENTATION_INTERVAL {
                        if let Some(rgb) = latest_frame.take() {
                            if sender.send(WorkerEvent::Frame(rgb)).is_err() { return Ok("Window closed.".into()); }
                            emitted = true;
                            last_emit_at = Instant::now();
                        }
                    }
                    if let Some(fault) = &report.execution.memory_fault {
                        if let Some(frame) = latest_frame.take() { let _ = sender.send(WorkerEvent::Frame(frame)); }
                        anyhow::bail!("Guest memory fault: {} at 0x{:08x}, PC=0x{:08x}{}",
                            fault.kind, fault.address, fault.pc, input_history_suffix(&input_history));
                    }
                    if let StopReason::CpuError(error) = &report.execution.stop_reason {
                        if let Some(frame) = latest_frame.take() { let _ = sender.send(WorkerEvent::Frame(frame)); }
                        anyhow::bail!("CPU stopped: {error}; PC=0x{:08x}; CPSR=0x{:08x}; SP=0x{:08x}; LR=0x{:08x}{}",
                            report.execution.pc, report.execution.cpsr, report.execution.sp, report.execution.lr, input_history_suffix(&input_history));
                    }
                    worker_diagnostics.record_poll(cpu_start.elapsed(), emitted);
                    last_report = Some(report);
                    std::thread::sleep(Duration::from_millis(1));
                }
                if let Some(rgb) = latest_frame { let _ = sender.send(WorkerEvent::Frame(rgb)); }
                Ok(last_report.map(|report| format!(
                    "Stopped. LCD frames: {}  PC: 0x{:08x}  Instructions: {}",
                    report.lcd_frames, report.execution.pc, report.execution.instructions
                )).unwrap_or_else(|| "Stopped.".into()))
            })();
            worker_files.fail_pending_commit("Phone stopped before the imported file could be saved.");
            let summary = match result {
                Ok(summary) => { worker_diagnostics.log("info", "worker", &summary); Ok(summary) },
                Err(error) => {
                    let summary = format!("{} error: {error:#}", paths.device);
                    worker_diagnostics.log("error", "worker", &summary);
                    Err(summary)
                },
            };
            worker_diagnostics.set_running(false);
            let _ = sender.send(WorkerEvent::Finished(summary));
        })?;
        Ok(Self { input, last_input: AtomicU32::new(u32::MAX), stop, handle: Some(handle), events, audio, diagnostics, files, card })
    }

    pub fn set_phone_keys(&self, keys: u32) {
        if self.last_input.swap(keys, Ordering::Relaxed) == keys { return; }
        let _ = self.input.send(WorkerCommand::Keys(keys));
    }

    pub fn inserted_memory_stick(&self) -> Option<&Path> { self.card.as_deref() }

    pub fn set_paused(&self, paused: bool) {
        if paused { self.last_input.store(0, Ordering::Relaxed); }
        let _ = self.input.send(WorkerCommand::SetPaused(paused));
    }

    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.input.send(WorkerCommand::Stop);
        if let Some(handle) = self.handle.take() { let _ = handle.join(); }
    }
}

impl Drop for Worker {
    fn drop(&mut self) { self.stop(); }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn service_codes_reach_the_firmware_input_queue_without_opening_host_ui() {
        use e830::PhoneKey::{Right, Star, Left, Hash, Zero, Six};
        let (input, receiver) = mpsc::channel();
        let (_, events) = event_channel();
        let worker = Worker { input, last_input: AtomicU32::new(u32::MAX),
            stop: Arc::new(AtomicBool::new(false)), handle: None, events,
            audio: AudioBuffer::default(), diagnostics: DiagnosticsHandle::default(),
            files: PhoneFiles::default(), card: None };
        for key in [Right, Star, Left, Left, Star, Left, Star, Star, Hash, Zero, Six, Hash] {
            worker.set_phone_keys(key.bit());
            worker.set_phone_keys(key.bit()); // A held contact has no second edge.
            worker.set_phone_keys(0);
            assert!(matches!(receiver.try_recv(), Ok(WorkerCommand::Keys(mask)) if mask == key.bit()));
            assert!(matches!(receiver.try_recv(), Ok(WorkerCommand::Keys(0))));
            assert!(receiver.try_recv().is_err());
            assert!(!worker.diagnostics.developer_mode());
        }
    }

    #[test]
    fn stalled_frontend_keeps_latest_panel_and_final_failure_in_order() {
        let (sender, events) = event_channel();
        for revision in 0..1000 {
            assert!(sender.send(WorkerEvent::Lcd(LcdFrame { width:1, height:1,
                rgb565_be:vec![0,0], revision })).is_ok());
            assert!(sender.send(WorkerEvent::Status(format!("{revision}"))).is_ok());
            assert!(sender.send(WorkerEvent::InputApplied(format!("input {revision}"))).is_ok());
        }
        assert!(sender.send(WorkerEvent::Finished(Err("firmware failure".into()))).is_ok());
        assert!(events.inner.lock().unwrap().queue.len() <= MAX_PENDING_EVENTS);
        let mut revision = None; let mut finished = false;
        while let Ok(event) = events.try_recv() {
            match event {
                WorkerEvent::Lcd(frame) => { assert!(!finished); revision = Some(frame.revision); },
                WorkerEvent::Finished(_) => { assert_eq!(revision, Some(999)); finished = true; },
                _ => {},
            }
        }
        assert!(finished);
        drop(sender);
        assert!(matches!(events.try_recv(), Err(mpsc::TryRecvError::Disconnected)));
    }
    #[test]
    fn closing_event_receiver_releases_queued_frames_and_notifies_worker() {
        let (sender, events) = event_channel();
        drop(events);
        assert!(sender.send(WorkerEvent::Status("closed".into())).is_err());
    }
}
