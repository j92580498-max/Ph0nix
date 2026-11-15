//! Opt-in host timings and sampled guest PCs. Never changes guest execution.
use std::cell::Cell;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use serde::Serialize;

const MAX_PC_BUCKETS: usize = 4096;

#[derive(Clone, Debug)]
pub struct ProfileConfig {
    pub sample_interval: u64,
    pub after_instruction: u64,
    pub before_instruction: u64,
}

impl Default for ProfileConfig {
    fn default() -> Self {
        Self { sample_interval: 4096, after_instruction: 0, before_instruction: u64::MAX }
    }
}

#[derive(Clone, Copy)]
pub(crate) enum Stage { Counter = 0, Diagnostics = 1, Devices = 2, Irq = 3, Loops = 4 }
const STAGES: [&str; 5] = ["instruction_counter", "diagnostics_and_hle", "device_updates", "irq_eligibility", "loop_optimization"];

#[derive(Default)]
struct Timing { nanos: [Cell<u64>; 5], samples: [Cell<u64>; 5] }

pub(crate) struct HookSample { timing: Rc<Timing>, stage: Stage, last: Instant }

impl HookSample {
    pub fn mark(&mut self, stage: Stage) {
        self.account();
        self.stage = stage;
    }
    fn account(&mut self) {
        let now = Instant::now();
        let i = self.stage as usize;
        self.timing.nanos[i].set(self.timing.nanos[i].get().saturating_add(nanos(now.duration_since(self.last))));
        self.timing.samples[i].set(self.timing.samples[i].get().saturating_add(1));
        self.last = now;
    }
}

impl Drop for HookSample { fn drop(&mut self) { self.account(); } }
fn nanos(duration: Duration) -> u64 { duration.as_nanos().min(u128::from(u64::MAX)) as u64 }

#[derive(Default, Serialize)]
struct MmioBucket { reads: u64, writes: u64, bytes: u64, host_nanoseconds: u64 }
#[derive(Serialize)]
struct PcBucket { address: String, samples: u64 }
#[derive(Serialize)]
struct StageTiming { samples: u64, host_nanoseconds: u64 }
#[derive(Serialize)]
struct Report {
    format_version: u32,
    notes: &'static str,
    sample_interval: u64,
    after_instruction: u64,
    before_instruction: u64,
    code_hook_calls: u64,
    sampled_hook_calls: u64,
    unrecorded_pc_samples: u64,
    run_calls: u64,
    emulation_host_nanoseconds: u64,
    final_guest_instruction: u64,
    fast_forwarded_guest_instructions: u64,
    stop_reason: String,
    stages: BTreeMap<&'static str, StageTiming>,
    hot_guest_regions: Vec<PcBucket>,
    mmio_banks: BTreeMap<String, MmioBucket>,
}

pub(crate) struct Profiler {
    config: ProfileConfig,
    output: Option<PathBuf>,
    timing: Rc<Timing>,
    hooks: u64,
    samples: u64,
    lost: u64,
    pcs: HashMap<u32, u64>,
    mmio: BTreeMap<u32, MmioBucket>,
    runs: u64,
    elapsed: u64,
    final_guest_instruction: u64,
    fast_forwarded: u64,
    stop_reason: String,
}

impl Profiler {
    pub fn new(mut config: ProfileConfig) -> Self {
        config.sample_interval = config.sample_interval.max(1);
        Self { config, output: None, timing: Rc::new(Timing::default()), hooks: 0, samples: 0,
            lost: 0, pcs: HashMap::new(), mmio: BTreeMap::new(), runs: 0, elapsed: 0,
            final_guest_instruction: 0, fast_forwarded: 0, stop_reason: String::new() }
    }
    pub fn from_env() -> Option<Self> {
        let output = std::env::var_os("E830_PROFILE_OUTPUT")?;
        let number = |name: &str, default| std::env::var(name).ok().and_then(|s| s.parse().ok()).unwrap_or(default);
        let mut profile = Self::new(ProfileConfig {
            sample_interval: number("E830_PROFILE_INTERVAL", 4096),
            after_instruction: number("E830_PROFILE_AFTER", 0),
            before_instruction: number("E830_PROFILE_BEFORE", u64::MAX),
        });
        profile.output = Some(output.into());
        Some(profile)
    }
    pub fn active(&self, instruction: u64) -> bool {
        instruction >= self.config.after_instruction && instruction < self.config.before_instruction
    }
    pub fn begin_hook(&mut self, pc: u32, instruction: u64) -> Option<HookSample> {
        if !self.active(instruction) { return None; }
        self.hooks += 1;
        if self.hooks % self.config.sample_interval != 0 { return None; }
        self.samples += 1;
        let bucket = pc & !0xff;
        if let Some(count) = self.pcs.get_mut(&bucket) { *count += 1; }
        else if self.pcs.len() < MAX_PC_BUCKETS { self.pcs.insert(bucket, 1); }
        else { self.lost += 1; }
        Some(HookSample { timing: Rc::clone(&self.timing), stage: Stage::Counter, last: Instant::now() })
    }
    pub fn mmio(&mut self, address: u32, size: usize, write: bool, duration: Duration) {
        let bucket = self.mmio.entry(address & !0xfff).or_default();
        if write { bucket.writes += 1; } else { bucket.reads += 1; }
        bucket.bytes += size as u64;
        bucket.host_nanoseconds = bucket.host_nanoseconds.saturating_add(nanos(duration));
    }
    pub fn finish_run(&mut self, duration: Duration, instruction: u64, skipped: u64, reason: String) {
        self.runs += 1;
        self.elapsed = self.elapsed.saturating_add(nanos(duration));
        self.final_guest_instruction = instruction;
        self.fast_forwarded = skipped;
        self.stop_reason = reason;
    }
    pub fn write(&self, path: &Path) -> std::io::Result<()> {
        // Exclusive creation: profiling must never overwrite a user file.
        let file = std::fs::OpenOptions::new().write(true).create_new(true).open(path)?;
        let mut hot: Vec<_> = self.pcs.iter().map(|(&pc, &samples)| PcBucket { address: format!("0x{pc:08x}"), samples }).collect();
        hot.sort_by(|a,b| b.samples.cmp(&a.samples).then(a.address.cmp(&b.address)));
        let report = Report {
            format_version: 1,
            notes: "PCs are periodic samples of real code-hook calls grouped into 256-byte regions, not function calls or skipped instructions. Periodic sampling may alias tight loops; change the interval to cross-check. Sampled section times are not exact CPU percentages and include instrumentation overhead. Emulation wall time includes all run windows; MMIO counts/times are restricted to the selected instruction window. No firmware bytes, RAM contents or phone numbers are exported.",
            sample_interval: self.config.sample_interval, after_instruction: self.config.after_instruction,
            before_instruction: self.config.before_instruction, code_hook_calls: self.hooks,
            sampled_hook_calls: self.samples, unrecorded_pc_samples: self.lost, run_calls: self.runs,
            emulation_host_nanoseconds: self.elapsed, final_guest_instruction: self.final_guest_instruction,
            fast_forwarded_guest_instructions: self.fast_forwarded, stop_reason: self.stop_reason.clone(),
            stages: STAGES.iter().enumerate().map(|(i,&name)| (name, StageTiming {
                samples: self.timing.samples[i].get(), host_nanoseconds: self.timing.nanos[i].get() })).collect(),
            hot_guest_regions: hot,
            mmio_banks: self.mmio.iter().map(|(&bank,b)| (format!("0x{bank:08x}"), MmioBucket {
                reads:b.reads, writes:b.writes, bytes:b.bytes, host_nanoseconds:b.host_nanoseconds })).collect(),
        };
        serde_json::to_writer_pretty(file, &report).map_err(std::io::Error::other)
    }
}

impl Drop for Profiler {
    fn drop(&mut self) {
        if let Some(path) = &self.output {
            match self.write(path) {
                Ok(()) => eprintln!("[e830-profile] saved: {}", path.display()),
                Err(error) => eprintln!("[e830-profile] could not save {}: {error}", path.display()),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_real_hooks_in_window_are_sampled_and_pc_storage_is_bounded() {
        let mut p=Profiler::new(ProfileConfig { sample_interval:2, after_instruction:10, before_instruction:20 });
        assert!(p.begin_hook(0x20000000,9).is_none());
        assert!(p.begin_hook(0x20000001,10).is_none());
        drop(p.begin_hook(0x200000ff,11));
        assert!(p.begin_hook(0x20000100,20).is_none());
        assert_eq!((p.hooks,p.samples,p.pcs.len()),(2,1,1));
        assert_eq!(p.pcs[&0x20000000],1);
        let mut p=Profiler::new(ProfileConfig { sample_interval:1,..Default::default() });
        for i in 0..MAX_PC_BUCKETS+5 { drop(p.begin_hook((i as u32)*256, i as u64)); }
        assert_eq!(p.pcs.len(),MAX_PC_BUCKETS);
        assert_eq!(p.lost,5);
    }
    #[test]
    fn early_return_accounts_current_stage_and_mmio_is_aggregated() {
        let mut p=Profiler::new(ProfileConfig { sample_interval:0,..Default::default() });
        let mut sample=p.begin_hook(0x20000000,0).unwrap();
        sample.mark(Stage::Devices); drop(sample);
        assert_eq!(p.timing.samples[Stage::Counter as usize].get(),1);
        assert_eq!(p.timing.samples[Stage::Devices as usize].get(),1);
        p.mmio(0x48000400,4,false,Duration::from_nanos(7));
        p.mmio(0x48000404,2,true,Duration::from_nanos(3));
        let bank=&p.mmio[&0x48000000];
        assert_eq!((bank.reads,bank.writes,bank.bytes,bank.host_nanoseconds),(1,1,6,10));
    }
    #[test]
    fn report_creation_never_overwrites_existing_file() {
        let path=std::env::temp_dir().join(format!("e830-profile-test-{}-{}.json",std::process::id(),std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        let mut p=Profiler::new(Default::default());
        p.finish_run(Duration::from_nanos(2), 100, 3, "InstructionBudget".into());
        p.finish_run(Duration::from_nanos(5), 200, 10, "Breakpoint".into());
        p.write(&path).unwrap();
        let original=std::fs::read(&path).unwrap();
        assert_eq!(p.write(&path).unwrap_err().kind(),std::io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read(&path).unwrap(),original);
        let report:serde_json::Value=serde_json::from_slice(&original).unwrap();
        assert_eq!(report["format_version"],1);
        assert_eq!(report["run_calls"],2);
        assert_eq!(report["emulation_host_nanoseconds"],7);
        assert_eq!(report["final_guest_instruction"],200);
        assert_eq!(report["fast_forwarded_guest_instructions"],10);
        assert_eq!(report["stop_reason"],"Breakpoint");
        std::fs::remove_file(path).unwrap();
    }
}
