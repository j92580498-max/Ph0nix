//! Native diagnostics opened explicitly from the application toolbar.
use std::time::{Duration, Instant};
use eframe::egui::{self, Color32, RichText};
use k750_native::worker::DiagnosticsHandle;
use serde_json::Value;

#[derive(Default, PartialEq)]
enum Tab { #[default] Metrics, Logs, Errors }

pub struct DeveloperWindow {
    open: bool,
    tab: Tab,
    next_snapshot: Instant,
    snapshot: Value,
    diagnostics: Option<DiagnosticsHandle>,
}

impl Default for DeveloperWindow {
    fn default() -> Self {
        Self { open: false, tab: Tab::Metrics, next_snapshot: Instant::now(), snapshot: Value::Null, diagnostics: None }
    }
}

impl DeveloperWindow {
    pub fn is_open(&self) -> bool { self.open }

    pub fn open(&mut self) {
        self.open = true;
        if let Some(handle) = &self.diagnostics { handle.set_developer_mode(true); }
        self.next_snapshot = Instant::now();
    }

    pub fn retain_diagnostics(&mut self, diagnostics: DiagnosticsHandle) {
        self.diagnostics = Some(diagnostics);
        self.next_snapshot = Instant::now();
    }

    pub fn show(&mut self, ctx: &egui::Context, handle: Option<&DiagnosticsHandle>) {
        if let Some(handle) = handle { self.diagnostics = Some(handle.clone()); }
        let handle = self.diagnostics.as_ref();
        if Instant::now() >= self.next_snapshot {
            self.next_snapshot = Instant::now() + Duration::from_millis(250);
            if let Some(handle) = handle {
                self.snapshot = serde_json::to_value(handle.snapshot()).unwrap_or(Value::Null);
                if self.snapshot["developer_mode"].as_bool() == Some(true) { self.open = true; }
            }
        }
        let was_open = self.open;
        egui::Window::new("ph0nix developer mode").id(egui::Id::new("ph0nix-developer"))
            .open(&mut self.open).default_size([620.0, 460.0]).show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut self.tab, Tab::Metrics, "Metrics");
                    ui.selectable_value(&mut self.tab, Tab::Logs, "Logs");
                    ui.selectable_value(&mut self.tab, Tab::Errors, "Errors");
                    if ui.button("Copy report").clicked() {
                        ui.output_mut(|output| output.copied_text = serde_json::to_string_pretty(&self.snapshot).unwrap_or_default());
                    }
                    if ui.button("Save report…").clicked() {
                        let report = serde_json::to_vec_pretty(&self.snapshot).unwrap_or_default();
                        let diagnostics = handle.cloned();
                        std::thread::spawn(move || {
                            let Some(path) = rfd::FileDialog::new().set_file_name("ph0nix-diagnostics.json")
                                .add_filter("Diagnostic report", &["json"]).save_file() else { return; };
                            if let Err(error) = std::fs::write(path, report) {
                                if let Some(handle) = diagnostics { handle.log("error", "desktop", &format!("Saving report: {error}")); }
                            }
                        });
                    }
                });
                ui.separator();
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    if self.tab == Tab::Metrics {
                        metrics(ui, &self.snapshot);
                    } else {
                        let entries = if self.tab == Tab::Errors && self.snapshot["error_logs"].is_array() {
                            &self.snapshot["error_logs"]
                        } else { &self.snapshot["logs"] };
                        let mut count = 0;
                        if let Some(entries) = entries.as_array() {
                            for entry in entries {
                                let level = entry["level"].as_str().unwrap_or("info");
                                if self.tab == Tab::Errors && !matches!(level, "error" | "warn") { continue; }
                                count += 1;
                                let color = match level { "error" => Color32::LIGHT_RED, "warn" => Color32::YELLOW, _ => Color32::LIGHT_GRAY };
                                ui.label(RichText::new(format!("{} ms [{level}] {}", entry["timestamp_ms"], entry["component"].as_str().unwrap_or(""))).monospace().color(color));
                                ui.label(RichText::new(entry["message"].as_str().unwrap_or("")).monospace());
                                ui.add_space(6.0);
                            }
                        }
                        if count == 0 { ui.label(if self.tab == Tab::Errors { "No errors recorded." } else { "No log entries yet." }); }
                    }
                });
            });
        if was_open && !self.open {
            if let Some(handle) = handle { handle.set_developer_mode(false); }
            self.snapshot["developer_mode"] = Value::Bool(false);
        }
    }
}

fn number(value: &Value, key: &str) -> String {
    value[key].as_f64().map(|number| format!("{number:.1}")).unwrap_or_else(|| "—".into())
}

fn memory(value: &Value, key: &str) -> String {
    value[key].as_u64().map(|bytes| format!("{:.1}", bytes as f64 / 1048576.0))
        .unwrap_or_else(|| "—".into())
}

fn metrics(ui: &mut egui::Ui, value: &Value) {
    egui::Grid::new("ph0nix-metrics").num_columns(2).striped(true).show(ui, |ui| {
        for (label, detail) in [
            ("Presentation", format!("{} FPS", number(value, "presentation_fps"))),
            ("Firmware LCD", format!("{} updates/s", number(value, "lcd_update_hz"))),
            ("Guest speed", format!("{} × real time", number(value, "guest_speed"))),
            ("Polling", format!("{} Hz", number(value, "poll_hz"))),
            ("Missed deadlines", value["missed_poll_deadlines"].to_string()),
            ("CPU app / engine", format!("{}% / {}%", number(value, "app_cpu_percent"), number(value, "engine_cpu_percent"))),
            ("Memory app / engine", format!("{} / {} MiB", memory(value, "app_memory_bytes"), memory(value, "engine_memory_bytes"))),
            ("Guest PC", format!("0x{:08X}", value["pc"].as_u64().unwrap_or(0))),
            ("Instructions/s", number(value, "instructions_per_second")),
        ] { ui.label(label); ui.monospace(detail); ui.end_row(); }
    });
    ui.add_space(10.0);
    ui.label("Timing: mean / p95 / max");
    for (key, label) in [("qmp_ms", "QMP"), ("qmp_receive_ms", "QMP receive"), ("qmp_decode_ms", "QMP decoding"),
        ("lcd_status_ms", "LCD status"), ("lcd_cached_ms", "Cached LCD"), ("frame_decode_ms", "LCD decoding"), ("render_ms", "Rendering"),
        ("cpu_quantum_ms", "CPU quantum"), ("audio_receive_ms", "Audio receive")] {
        if value[key].is_object() {
            if value[key]["samples"].as_u64() == Some(0) {
                ui.monospace(format!("{label}: —"));
            } else {
                ui.monospace(format!("{label}: {} / {} / {} ms", number(&value[key], "mean_ms"),
                    number(&value[key], "p95_ms"), number(&value[key], "max_ms")));
            }
        }
    }
    ui.add_space(10.0);
    ui.monospace(format!("Audio frames: {} · dropped: {} · invalid connections: {}",
        value["audio_frames"], value["audio_dropped_frames"], value["audio_invalid_connections"]));
    if let Some(components) = value["engine_components"].as_object() {
        ui.add_space(10.0); ui.label("Engine timers (sampled host costs)");
        for (name, component) in components {
            ui.monospace(format!("{name}: {} service calls · {:.2} µs/call · {} unchanged deadlines",
                component["service_calls"], component["service_mean_us"].as_f64().unwrap_or(0.0), component["unchanged_deadlines"]));
        }
    }
    ui.add_space(10.0);
    ui.label(RichText::new("CPU is measured per process; 100% uses one core. Firmware update rate depends on the game.").small());
}
