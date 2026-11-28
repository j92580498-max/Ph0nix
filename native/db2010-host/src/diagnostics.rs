// SPDX-License-Identifier: GPL-2.0-or-later
//! Cheap, bounded diagnostics shared by the native hosts and their UI threads.
use serde::Serialize;
use std::{collections::{BTreeMap, VecDeque}, sync::{Arc, Mutex}, time::{Duration, Instant}};

const LOG_LIMIT: usize = 128;
const ERROR_LIMIT: usize = 64;
const LOG_BYTES: usize = 2048;
const TIMING_LIMIT: usize = 128;
const RATE_INTERVAL: Duration = Duration::from_secs(1);
const PROCESS_INTERVAL: Duration = Duration::from_millis(500);

#[derive(Clone, Debug, Serialize)]
pub struct DiagnosticLog {
    pub timestamp_ms: u64,
    pub level: String,
    pub component: String,
    pub message: String,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct TimingSnapshot {
    pub samples: u64,
    pub mean_ms: f64,
    pub max_ms: f64,
    pub p95_ms: f64,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct EngineComponentProfile {
    pub reschedules: u64,
    pub unchanged_deadlines: u64,
    pub service_calls: u64,
    pub schedule_samples: u64,
    pub schedule_ns: u64,
    pub schedule_mean_us: f64,
    pub service_samples: u64,
    pub service_ns: u64,
    pub service_mean_us: f64,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct DiagnosticsSnapshot {
    pub developer_mode: bool,
    pub uptime_seconds: f64,
    pub running: bool,
    pub guest_virtual_ns: u64,
    pub guest_speed: Option<f64>,
    pub instructions_per_second: Option<f64>,
    pub pc: Option<u32>,
    /// Actual UI presents, reported by the frontend, including repeated panels.
    pub presentation_fps: f64,
    pub poll_hz: f64,
    /// New complete panels received, never promoted to presentation FPS.
    pub lcd_update_hz: f64,
    /// 100% means one fully occupied host CPU core; multi-core totals can exceed it.
    pub app_cpu_percent: Option<f64>,
    pub engine_cpu_percent: Option<f64>,
    pub app_memory_bytes: Option<u64>,
    pub engine_memory_bytes: Option<u64>,
    pub poll_ms: TimingSnapshot,
    pub qmp_ms: TimingSnapshot,
    /// Response read calls, including engine/socket waits; excludes JSON decode.
    pub qmp_receive_ms: TimingSnapshot,
    /// Envelope and property JSON decodes, each measured separately.
    pub qmp_decode_ms: TimingSnapshot,
    pub lcd_transport_ms: TimingSnapshot,
    pub lcd_status_ms: TimingSnapshot,
    pub lcd_cached_ms: TimingSnapshot,
    pub progress_ms: TimingSnapshot,
    pub frame_decode_ms: TimingSnapshot,
    pub render_ms: TimingSnapshot,
    pub cpu_quantum_ms: TimingSnapshot,
    /// PCM envelope parsing and enqueueing; excludes waits for the next packet.
    pub audio_receive_ms: TimingSnapshot,
    pub qmp_received_bytes: u64,
    pub polls: u64,
    pub lcd_updates: u64,
    pub presentations: u64,
    pub missed_poll_deadlines: u64,
    pub errors: u64,
    pub audio_packets: u64,
    pub audio_frames: u64,
    pub audio_nonzero_samples: u64,
    pub audio_dropped_frames: u64,
    pub audio_invalid_connections: u64,
    pub audio_queued_frames: u64,
    pub audio_queued_bytes: usize,
    pub engine_components: BTreeMap<String, EngineComponentProfile>,
    pub logs: Vec<DiagnosticLog>,
    pub error_logs: Vec<DiagnosticLog>,
}

#[derive(Default)]
struct Timings { samples: u64, recent: VecDeque<f64> }
impl Timings {
    fn add(&mut self, elapsed: Duration) {
        self.samples = self.samples.saturating_add(1);
        if self.recent.len() == TIMING_LIMIT { self.recent.pop_front(); }
        self.recent.push_back(elapsed.as_secs_f64() * 1000.0);
    }
    fn snapshot(&self) -> TimingSnapshot {
        if self.recent.is_empty() { return TimingSnapshot::default(); }
        let mut sorted: Vec<_> = self.recent.iter().copied().collect();
        sorted.sort_unstable_by(f64::total_cmp);
        TimingSnapshot {
            samples: self.samples,
            mean_ms: sorted.iter().sum::<f64>() / sorted.len() as f64,
            max_ms: *sorted.last().unwrap(),
            p95_ms: sorted[(sorted.len() * 95).div_ceil(100).saturating_sub(1)],
        }
    }
}

struct State {
    started: Instant,
    rate_at: Instant,
    process_at: Instant,
    rate_counts: (u64, u64, u64),
    guest_at: Option<Instant>,
    guest_counts: (u64, u64),
    observations: u64,
    has_guest_clock: bool,
    has_observations: bool,
    value: DiagnosticsSnapshot,
    logs: VecDeque<DiagnosticLog>,
    error_logs: VecDeque<DiagnosticLog>,
    poll: Timings, qmp: Timings, qmp_receive: Timings, qmp_decode: Timings,
    lcd_transport: Timings, lcd_status: Timings, lcd_cached: Timings, progress: Timings,
    frame_decode: Timings, render: Timings, cpu_quantum: Timings, audio_receive: Timings,
    engine_pid: Option<u32>,
    processes: ProcessMonitor,
}
impl Default for State {
    fn default() -> Self {
        let now = Instant::now();
        Self {
            started: now, rate_at: now, process_at: now - PROCESS_INTERVAL,
            rate_counts: (0, 0, 0), guest_at: None, guest_counts: (0, 0), observations: 0,
            has_guest_clock: false, has_observations: false,
            value: DiagnosticsSnapshot::default(),
            logs: VecDeque::with_capacity(LOG_LIMIT), error_logs: VecDeque::with_capacity(ERROR_LIMIT),
            poll: Timings::default(), qmp: Timings::default(),
            qmp_receive: Timings::default(), qmp_decode: Timings::default(),
            lcd_transport: Timings::default(), progress: Timings::default(), frame_decode: Timings::default(),
            lcd_status: Timings::default(), lcd_cached: Timings::default(),
            render: Timings::default(), cpu_quantum: Timings::default(), audio_receive: Timings::default(),
            engine_pid: None, processes: ProcessMonitor::default(),
        }
    }
}
impl State {
    fn update_rates(&mut self, now: Instant) {
        let elapsed = now.saturating_duration_since(self.rate_at);
        if elapsed < RATE_INTERVAL { return; }
        let seconds = elapsed.as_secs_f64();
        let (polls, lcd, presents) = self.rate_counts;
        self.value.poll_hz = self.value.polls.saturating_sub(polls) as f64 / seconds;
        self.value.lcd_update_hz = self.value.lcd_updates.saturating_sub(lcd) as f64 / seconds;
        self.value.presentation_fps = self.value.presentations.saturating_sub(presents) as f64 / seconds;
        self.rate_at = now;
        self.rate_counts = (self.value.polls, self.value.lcd_updates, self.value.presentations);
    }
    fn update_guest_rates(&mut self, now: Instant) {
        if let Some(at) = self.guest_at {
            let seconds = now.saturating_duration_since(at).as_secs_f64();
            if seconds < RATE_INTERVAL.as_secs_f64() { return; }
            self.value.guest_speed = self.has_guest_clock.then(||
                self.value.guest_virtual_ns.saturating_sub(self.guest_counts.0) as f64 / 1e9 / seconds);
            self.value.instructions_per_second = self.has_observations.then(||
                self.observations.saturating_sub(self.guest_counts.1) as f64 / seconds);
        }
        self.guest_at = Some(now);
        self.guest_counts = (self.value.guest_virtual_ns, self.observations);
    }
}

fn bounded(text: &str, limit: usize) -> String {
    let mut end = text.len().min(limit);
    while !text.is_char_boundary(end) { end -= 1; }
    text[..end].to_owned()
}

#[derive(Clone, Default)]
pub struct DiagnosticsHandle { inner: Arc<Mutex<State>> }
impl DiagnosticsHandle {
    fn with<T>(&self, operation: impl FnOnce(&mut State) -> T) -> T {
        operation(&mut self.inner.lock().unwrap_or_else(|error| error.into_inner()))
    }
    pub fn snapshot(&self) -> DiagnosticsSnapshot {
        // Frontends also show diagnostics before boot and after a stop.
        // Refresh host counters even when no guest progress is arriving.
        self.refresh_processes();
        self.with(|s| {
            let now = Instant::now();
            s.update_rates(now);
            let mut value = s.value.clone();
            value.uptime_seconds = now.saturating_duration_since(s.started).as_secs_f64();
            value.poll_ms = s.poll.snapshot(); value.qmp_ms = s.qmp.snapshot();
            value.qmp_receive_ms = s.qmp_receive.snapshot(); value.qmp_decode_ms = s.qmp_decode.snapshot();
            value.lcd_transport_ms = s.lcd_transport.snapshot(); value.progress_ms = s.progress.snapshot();
            value.lcd_status_ms = s.lcd_status.snapshot(); value.lcd_cached_ms = s.lcd_cached.snapshot();
            value.frame_decode_ms = s.frame_decode.snapshot(); value.render_ms = s.render.snapshot();
            value.cpu_quantum_ms = s.cpu_quantum.snapshot(); value.audio_receive_ms = s.audio_receive.snapshot();
            value.logs = s.logs.iter().cloned().collect();
            value.error_logs = s.error_logs.iter().cloned().collect();
            value
        })
    }
    pub fn log(&self, level: &str, component: &str, message: impl AsRef<str>) {
        self.with(|s| {
            if level == "error" { s.value.errors = s.value.errors.saturating_add(1); }
            if s.logs.len() == LOG_LIMIT { s.logs.pop_front(); }
            let entry = DiagnosticLog {
                timestamp_ms: s.started.elapsed().as_millis().min(u64::MAX as u128) as u64,
                level: bounded(level, 16), component: bounded(component, 64), message: bounded(message.as_ref(), LOG_BYTES),
            };
            if matches!(level, "error" | "warn") {
                if s.error_logs.len() == ERROR_LIMIT { s.error_logs.pop_front(); }
                s.error_logs.push_back(entry.clone());
            }
            s.logs.push_back(entry);
        });
    }
    pub(crate) fn record_save_phase(&self, phase: &str, elapsed: Duration, ok: bool) {
        self.log("info", "save-timing", format!("{phase} elapsed_ms={:.3} ok={ok}",
            elapsed.as_secs_f64() * 1000.0));
    }
    pub(crate) fn measure_save<T, E>(&self, phase: &str, operation: impl FnOnce() -> Result<T, E>) -> Result<T, E> {
        let start = Instant::now();
        let result = operation();
        self.record_save_phase(phase, start.elapsed(), result.is_ok());
        result
    }
    pub fn set_developer_mode(&self, enabled: bool) {
        self.with(|s| s.value.developer_mode = enabled);
    }
    pub fn developer_mode(&self) -> bool { self.with(|s| s.value.developer_mode) }
    pub fn record_presentation(&self) { self.record_presentations(1); }
    pub fn record_presentations(&self, count: u64) {
        self.with(|s| { s.value.presentations = s.value.presentations.saturating_add(count); s.update_rates(Instant::now()); });
    }
    pub fn record_render(&self, elapsed: Duration) { self.with(|s| s.render.add(elapsed)); }
    pub fn record_poll(&self, elapsed: Duration, lcd_changed: bool) {
        self.with(|s| {
            s.value.polls = s.value.polls.saturating_add(1);
            s.value.lcd_updates = s.value.lcd_updates.saturating_add(u64::from(lcd_changed));
            s.poll.add(elapsed); s.update_rates(Instant::now());
        });
    }
    pub fn record_missed_deadlines(&self, count: u64) {
        self.with(|s| s.value.missed_poll_deadlines = s.value.missed_poll_deadlines.saturating_add(count));
    }
    pub fn record_qmp(&self, command: &str, property: Option<&str>, elapsed: Duration, bytes: usize) {
        self.with(|s| {
            s.qmp.add(elapsed);
            s.value.qmp_received_bytes = s.value.qmp_received_bytes.saturating_add(bytes as u64);
            if command == "qom-get" {
                match property {
                    Some("display-status") => {s.lcd_transport.add(elapsed);s.lcd_status.add(elapsed);},
                    Some("display-cached") => {s.lcd_transport.add(elapsed);s.lcd_cached.add(elapsed);},
                    Some("progress") => s.progress.add(elapsed),
                    _ => {},
                }
            }
        });
    }
    pub(crate) fn record_qmp_receive(&self, elapsed: Duration) { self.with(|s| s.qmp_receive.add(elapsed)); }
    pub(crate) fn record_qmp_decode(&self, elapsed: Duration) { self.with(|s| s.qmp_decode.add(elapsed)); }
    pub(crate) fn record_lcd_transport(&self, elapsed: Duration) { self.with(|s| s.lcd_transport.add(elapsed)); }
    pub fn record_frame_decode(&self, elapsed: Duration) { self.with(|s| s.frame_decode.add(elapsed)); }
    pub fn record_cpu_quantum(&self, elapsed: Duration) { self.with(|s| s.cpu_quantum.add(elapsed)); }
    pub fn update_guest(&self, virtual_ns: Option<u64>, observations: Option<u64>, pc: Option<u32>, running: bool) {
        self.with(|s| {
            // Boot/restore clocks can begin at a large offset. Establish a
            // baseline before reporting rates instead of counting that offset.
            if let Some(time) = virtual_ns {
                if !s.has_guest_clock || time < s.value.guest_virtual_ns { s.guest_at = None; }
                s.value.guest_virtual_ns = time; s.has_guest_clock = true;
            }
            if let Some(count) = observations {
                if !s.has_observations || count < s.observations { s.guest_at = None; }
                s.observations = count; s.has_observations = true;
            }
            s.value.pc = pc; s.value.running = running;
            s.update_guest_rates(Instant::now());
            s.update_rates(Instant::now());
        });
        self.refresh_processes();
    }
    pub fn set_running(&self, running: bool) { self.with(|s| s.value.running = running); }
    pub fn update_engine_components(&self, profile: &serde_json::Value) {
        let Some(components) = profile.as_object() else { return; };
        self.with(|s| {
            s.value.engine_components.clear();
            for (name, value) in components.iter().take(32) {
                let number = |field: &str| value[field].as_u64().unwrap_or(0);
                let schedule_samples = number("schedule_samples");
                let schedule_ns = number("schedule_ns");
                let service_samples = number("service_samples");
                let service_ns = number("service_ns");
                s.value.engine_components.insert(bounded(name, 64), EngineComponentProfile {
                    reschedules: number("reschedules"), unchanged_deadlines: number("unchanged_deadlines"),
                    service_calls: number("service_calls"), schedule_samples, schedule_ns,
                    schedule_mean_us: if schedule_samples == 0 { 0.0 } else { schedule_ns as f64 / schedule_samples as f64 / 1000.0 },
                    service_samples, service_ns,
                    service_mean_us: if service_samples == 0 { 0.0 } else { service_ns as f64 / service_samples as f64 / 1000.0 },
                });
            }
        });
    }
    pub fn set_engine_pid(&self, pid: Option<u32>) {
        self.with(|s| { s.engine_pid = pid; s.processes.engine = None; s.process_at = Instant::now() - PROCESS_INTERVAL; });
        self.refresh_processes();
    }
    pub fn refresh_processes(&self) {
        self.with(|s| {
            let now = Instant::now();
            if now.saturating_duration_since(s.process_at) < PROCESS_INTERVAL { return; }
            s.process_at = now;
            let app = s.processes.sample(std::process::id(), now, false);
            let engine = s.engine_pid.and_then(|pid| s.processes.sample(pid, now, true));
            s.value.app_cpu_percent = app.and_then(|v| v.0);
            s.value.app_memory_bytes = app.map(|v| v.1);
            s.value.engine_cpu_percent = engine.and_then(|v| v.0);
            s.value.engine_memory_bytes = engine.map(|v| v.1);
        });
    }
    pub(crate) fn record_audio(&self, stats: &crate::audio::AudioStats, queued_frames: u64, queued_bytes: usize) {
        self.with(|s| {
            s.value.audio_packets = stats.packets; s.value.audio_frames = stats.frames;
            s.value.audio_nonzero_samples = stats.nonzero_samples; s.value.audio_dropped_frames = stats.dropped_frames;
            s.value.audio_invalid_connections = stats.invalid_connections;
            s.value.audio_queued_frames = queued_frames; s.value.audio_queued_bytes = queued_bytes;
        });
    }
    pub(crate) fn record_audio_receive(&self, elapsed: Duration) { self.with(|s| s.audio_receive.add(elapsed)); }
}

struct ProcessMonitor {
    app: Option<(Instant, f64)>, engine: Option<(Instant, f64)>,
    #[cfg(any(target_os = "linux", target_os = "android"))]
    ticks_per_second: f64,
    #[cfg(any(target_os = "linux", target_os = "android"))]
    page_size: u64,
}
impl Default for ProcessMonitor {
    fn default() -> Self {
        Self {
            app: None, engine: None,
            #[cfg(any(target_os = "linux", target_os = "android"))]
            ticks_per_second: unsafe { libc::sysconf(libc::_SC_CLK_TCK) }.max(1) as f64,
            #[cfg(any(target_os = "linux", target_os = "android"))]
            page_size: unsafe { libc::sysconf(libc::_SC_PAGESIZE) }.max(1) as u64,
        }
    }
}
impl ProcessMonitor {
    fn sample(&mut self, pid: u32, now: Instant, engine: bool) -> Option<(Option<f64>, u64)> {
        let (cpu_seconds, rss) = self.read(pid)?;
        let previous = if engine { &mut self.engine } else { &mut self.app };
        let percent = previous.and_then(|(at, cpu)| {
            let elapsed = now.saturating_duration_since(at).as_secs_f64();
            (elapsed > 0.0 && cpu_seconds >= cpu).then_some((cpu_seconds - cpu) / elapsed * 100.0)
        });
        *previous = Some((now, cpu_seconds));
        Some((percent, rss))
    }
    #[cfg(any(target_os = "linux", target_os = "android"))]
    fn read(&self, pid: u32) -> Option<(f64, u64)> {
        let text = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let fields: Vec<_> = text.rsplit_once(") ")?.1.split_whitespace().collect();
        let user: u64 = fields.get(11)?.parse().ok()?;
        let system: u64 = fields.get(12)?.parse().ok()?;
        let rss: i64 = fields.get(21)?.parse().ok()?;
        Some((user.saturating_add(system) as f64 / self.ticks_per_second, rss.max(0) as u64 * self.page_size))
    }
    #[cfg(windows)]
    fn read(&self, pid: u32) -> Option<(f64, u64)> { windows_process(pid) }
    #[cfg(not(any(target_os = "linux", target_os = "android", windows)))]
    fn read(&self, _pid: u32) -> Option<(f64, u64)> { None }
}

#[cfg(windows)]
fn windows_process(pid: u32) -> Option<(f64, u64)> {
    use std::ffi::c_void;
    #[repr(C)]
    #[derive(Default)]
    struct FileTime { low: u32, high: u32 }
    impl FileTime { fn seconds(&self) -> f64 { ((self.high as u64) << 32 | self.low as u64) as f64 / 10_000_000.0 } }
    #[repr(C)]
    #[derive(Default)]
    struct Memory { cb: u32, faults: u32, peak: usize, working: usize, quota_peak: usize, quota: usize,
        nonpaged_peak: usize, nonpaged: usize, pagefile: usize, pagefile_peak: usize }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut c_void;
        fn CloseHandle(handle: *mut c_void) -> i32;
        fn GetProcessTimes(process: *mut c_void, created: *mut FileTime, exited: *mut FileTime,
            kernel: *mut FileTime, user: *mut FileTime) -> i32;
    }
    #[link(name = "psapi")]
    unsafe extern "system" { fn GetProcessMemoryInfo(process: *mut c_void, memory: *mut Memory, size: u32) -> i32; }
    unsafe {
        let handle = OpenProcess(0x0400 | 0x0010, 0, pid);
        if handle.is_null() { return None; }
        let mut created = FileTime::default(); let mut exited = FileTime::default();
        let mut kernel = FileTime::default(); let mut user = FileTime::default();
        let mut memory = Memory { cb: std::mem::size_of::<Memory>() as u32, ..Memory::default() };
        let times = GetProcessTimes(handle, &mut created, &mut exited, &mut kernel, &mut user);
        let size = memory.cb;
        let got_memory = GetProcessMemoryInfo(handle, &mut memory, size);
        CloseHandle(handle);
        (times != 0 && got_memory != 0).then_some((kernel.seconds() + user.seconds(), memory.working as u64))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn log_and_timing_memory_remain_bounded() {
        let handle = DiagnosticsHandle::default();
        for _ in 0..300 { handle.log("error", "test", "é".repeat(LOG_BYTES)); handle.record_poll(Duration::from_millis(2), true); }
        let snapshot = handle.snapshot();
        assert_eq!(snapshot.logs.len(), LOG_LIMIT);
        assert_eq!(snapshot.error_logs.len(), ERROR_LIMIT);
        assert_eq!(snapshot.logs[0].message.len(), LOG_BYTES);
        assert_eq!(snapshot.errors, 300);
        assert_eq!(snapshot.poll_ms.samples, 300);
        assert_eq!(snapshot.poll_ms.mean_ms, 2.0);
        assert_eq!(handle.with(|s| s.poll.recent.len()), TIMING_LIMIT);
    }
    #[test]
    fn errors_survive_routine_log_eviction() {
        let handle = DiagnosticsHandle::default();
        handle.log("error", "engine", "Guest stopped during sound setup");
        handle.log("warn", "audio", "No output device");
        for _ in 0..LOG_LIMIT { handle.log("info", "keypad", "Contact released"); }
        let snapshot = handle.snapshot();
        assert!(snapshot.logs.iter().all(|entry| entry.level == "info"));
        assert_eq!(snapshot.error_logs.len(), 2);
        assert_eq!(snapshot.error_logs[0].message, "Guest stopped during sound setup");
        assert_eq!(snapshot.error_logs[1].level, "warn");
        assert_eq!(snapshot.errors, 1);
    }
    #[test]
    fn rate_metrics_keep_presentation_guest_and_lcd_counts_distinct() {
        let handle = DiagnosticsHandle::default();
        handle.update_guest(Some(900_000_000_000), Some(8_000_000_000), Some(0x4400_0000), true);
        handle.with(|s| {
            s.value.polls = 60; s.value.lcd_updates = 12; s.value.presentations = 60;
            s.value.guest_virtual_ns += 500_000_000; s.observations += 2_000_000;
            s.update_rates(s.rate_at + Duration::from_secs(1));
            s.update_guest_rates(s.guest_at.unwrap() + Duration::from_secs(1));
        });
        let snapshot = handle.snapshot();
        assert_eq!(snapshot.poll_hz, 60.0); assert_eq!(snapshot.lcd_update_hz, 12.0);
        assert_eq!(snapshot.presentation_fps, 60.0); assert_eq!(snapshot.guest_speed, Some(0.5));
        assert_eq!(snapshot.instructions_per_second, Some(2_000_000.0));
    }
    #[cfg(any(target_os = "linux", target_os = "android", windows))]
    #[test]
    fn process_monitor_reads_actual_host_memory_and_cpu_counters() {
        let monitor = ProcessMonitor::default();
        let (cpu, rss) = monitor.read(std::process::id()).unwrap();
        assert!(cpu >= 0.0); assert!(rss > 0);
    }
    #[cfg(any(target_os = "linux", target_os = "android", windows))]
    #[test]
    fn snapshots_report_host_resources_without_running_a_guest() {
        let handle = DiagnosticsHandle::default();
        assert!(handle.snapshot().app_memory_bytes.unwrap() > 0);
        handle.with(|state| state.process_at = Instant::now() - PROCESS_INTERVAL);
        assert!(handle.snapshot().app_cpu_percent.unwrap() >= 0.0);
        assert!(handle.snapshot().engine_memory_bytes.is_none());
    }
}
