// Derived from tayga16/SamsungE380-Emu; original Apache-2.0/MIT terms retained.
//! Profile-aware firmware launcher preserving the original egui desktop layout.
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, Rect, RichText, Sense, Vec2};
use serde::{Deserialize, Serialize};
use e830::PhoneKey;

use k750_native::worker::{FirmwareSelection, Worker, WorkerEvent};
use e830::devices::{registry::Registry, runtime::DeviceInfo};

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub show_fps: bool,
    pub sound: bool,
    pub fullscreen: bool,
    pub last_import_dir: Option<PathBuf>,
    pub firmware: Option<FirmwareSelection>,
    pub device: String,
    pub selections: std::collections::BTreeMap<String, FirmwareSelection>,
    pub library: Vec<FirmwareSelection>,
    pub grid: bool,
    pub sort_by_device: bool,
    pub resume: bool,
    pub library_initialized: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self { show_fps: true, sound: true, fullscreen: false, last_import_dir: None, firmware: None, device: "k750i".into(), selections: Default::default(), library: Vec::new(), grid: true, sort_by_device: false, resume: true, library_initialized: false }
    }
}

impl Settings {
    pub fn load(root: &std::path::Path) -> Self {
        std::fs::read(root.join("featurephone-config.json")).ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default()
    }
}

#[derive(PartialEq)]
enum Screen { Library, Devices, Settings, Run }

enum FileDialogEvent {
    Firmware(Result<FirmwareSelection, String>),
    Nand(Option<PathBuf>),
}

pub struct PhoneLauncher {
    root: PathBuf,
    settings_dir: PathBuf,
    registry: Registry,
    config: Settings,
    config_draft: Option<Settings>,
    screen: Screen,
    worker: Option<Worker>,
    audio_output: Option<crate::audio::AudioOutput>,
    audio_error: Option<String>,
    events_tx: Sender<FileDialogEvent>,
    events_rx: Receiver<FileDialogEvent>,
    dialog_open: bool,
    running: bool,
    status: String,
    last_frame_texture: Option<egui::TextureHandle>,
    brand_logo: Option<egui::TextureHandle>,
    last_frame_status: Option<String>,
    frame_stats: FrameStats,
    sent_keys: u32,
    search: String,
    stopping: Option<std::thread::JoinHandle<Option<Result<String, String>>>>,
    pending_run: Option<FirmwareSelection>,
    developer: crate::developer::DeveloperWindow,
    files: crate::files::FileTools,
}

impl PhoneLauncher {
    pub fn new(root: PathBuf, mut config: Settings) -> Self {
        let registry = k750_native::registry();
        config.library.retain(|selection| registry.device(&selection.device).is_ok());
        config.selections.retain(|device, _| registry.device(device).is_ok());
        if config.firmware.as_ref().is_some_and(|selection| registry.device(&selection.device).is_err()) {
            config.firmware = None;
        }
        if registry.device(&config.device).is_err() { config.device = "k750i".into(); }
        if config.firmware.is_none() {
            config.firmware = config.selections.get(&config.device).cloned().or_else(|| {
                let info = registry.device(&config.device).ok()?.info();
                registry.loader(info.default_loader).ok()?.bundled(&root)
                    .map(|files| FirmwareSelection { device: config.device.clone(), loader: None, files })
            });
        }
        if let Some(paths) = config.firmware.as_ref() {
            config.device = paths.device.clone();
            config.selections.insert(paths.device.clone(), paths.clone());
        }
        if !config.library_initialized {
            if config.library.is_empty() { config.library = config.selections.values().cloned().collect(); }
            config.library_initialized = true;
        }
        let (events_tx, events_rx) = mpsc::channel();
        Self {
            settings_dir: root.clone(), root, registry, config, config_draft: None, screen: Screen::Library,
            worker: None, audio_output: None, audio_error: None, events_tx, events_rx, dialog_open: false, running: false,
            status: "Welcome to ph0nix.".into(),
            last_frame_texture: None, last_frame_status: None, frame_stats: FrameStats::default(),
            brand_logo: None,
            sent_keys: 0,
            search: String::new(), stopping: None, pending_run: None,
            developer: crate::developer::DeveloperWindow::default(),
            files: crate::files::FileTools::default(),
        }
    }

    pub fn with_settings_dir(mut self, directory: PathBuf) -> Self {
        self.settings_dir = directory;
        self
    }

    fn save_settings(&mut self) {
        if let Some(paths) = self.config.firmware.as_ref() {
            self.config.selections.insert(paths.device.clone(), paths.clone());
        }
        let result = serde_json::to_vec_pretty(&self.config).map_err(anyhow::Error::from)
            .and_then(|bytes| {
                std::fs::create_dir_all(&self.settings_dir)?;
                std::fs::write(self.settings_dir.join("featurephone-config.json"), bytes).map_err(anyhow::Error::from)
            });
        if let Err(error) = result { self.status = format!("Could not save settings: {error}"); }
    }

    pub fn run_selected_firmware(&mut self) {
        if let Some(paths) = self.config.firmware.clone() { self.spawn_profile_run(paths); }
    }

    fn stop(&mut self) {
        self.audio_output = None;
        self.audio_error = None;
        if let Some(mut worker) = self.worker.take() {
            self.developer.retain_diagnostics(worker.diagnostics.clone());
            self.stopping = Some(std::thread::spawn(move || {
                worker.stop();
                while let Ok(event) = worker.events.try_recv() {
                    if let WorkerEvent::Finished(result) = event { return Some(result); }
                }
                None
            }));
        }
        self.running = false;
        self.sent_keys = 0;
    }

    fn spawn_profile_run(&mut self, paths: FirmwareSelection) {
        if let Err(error) = paths.prepared_files(&self.registry) {
            self.status = error.to_string();
            return;
        }
        let name = self.registry.device(&paths.device).map(|profile| profile.info().name).unwrap_or("Phone");
        self.stop();
        if self.stopping.is_some() {
            self.pending_run = Some(paths);
            self.status = "Saving the previous phone state…".into();
            return;
        }
        self.config.last_import_dir = paths.files.image.parent().map(|path| path.to_path_buf());
        self.config.device = paths.device.clone();
        self.config.selections.insert(paths.device.clone(), paths.clone());
        self.config.firmware = Some(paths.clone());
        if let Some(entry) = self.config.library.iter_mut().find(|entry| entry.device == paths.device && entry.files.image == paths.files.image) {
            *entry = paths.clone();
        } else { self.config.library.push(paths.clone()); }
        self.save_settings();
        self.last_frame_texture = None;
        self.last_frame_status = None;
        self.frame_stats = FrameStats::default();
        self.screen = Screen::Run;
        let options = k750_native::worker::HostOptions::discover(&self.root).ok().map(|mut options| {
            options.resume = self.config.resume; options
        });
        let card = if paths.device == "k750i" { self.files.boot_card().map(std::path::Path::to_path_buf) } else { None };
        match Worker::start_with_card(paths, options, None, card) {
            Ok(worker) => {
                let diagnostics = worker.diagnostics.clone();
                self.worker = Some(worker);
                if self.config.sound {
                    match crate::audio::AudioOutput::new(diagnostics.clone()) {
                        Ok(output) => self.audio_output = Some(output),
                        Err(error) => {
                            diagnostics.log("error", "desktop-audio", error.to_string());
                            self.audio_error = Some(format!("Sound unavailable: {error}"));
                        },
                    }
                }
                self.running = true;
                self.status = format!("Starting {name} firmware...");
            }
            Err(error) => { self.status = format!("Could not start {name}: {error}"); }
        }
    }

    fn spawn_firmware_file_dialog(&mut self) {
        if self.dialog_open { return; }
        let Some(info) = self.selected_info() else {
            self.status = format!("Unknown phone profile {}", self.config.device); return;
        };
        let loader = self.registry.loader(info.default_loader).unwrap();
        let extensions: Vec<_> = loader.roles().iter().flat_map(|role| role.extensions.iter().copied()).collect();
        let hint = loader.selection_hint();
        let device = self.config.device.clone();
        self.dialog_open = true;
        let directory = self.config.last_import_dir.clone().unwrap_or_else(|| self.root.join("firmware"));
        let sender = self.events_tx.clone();
        std::thread::spawn(move || {
            let selection = rfd::FileDialog::new()
                .set_title(format!("Select {} firmware files", info.name))
                .add_filter("Phone firmware", &extensions)
                .set_directory(directory).pick_files();
            let result = selection.map(|paths| FirmwareSelection::classify(&k750_native::registry(), &device, paths)
                .map_err(|error| error.to_string())).unwrap_or_else(|| Err("cancelled".into()));
            let _ = sender.send(FileDialogEvent::Firmware(result));
        });
        self.status = hint.into();
    }

    fn selected_info(&self) -> Option<DeviceInfo> {
        self.registry.device(&self.config.device).ok().map(|profile| profile.info())
    }

    fn select_device(&mut self, device: String) {
        if self.registry.device(&device).is_err() {
            self.status = "This phone profile is unavailable.".into();
            return;
        }
        self.stop();
        if let Some(paths) = self.config.firmware.take() {
            self.config.selections.insert(paths.device.clone(), paths);
        }
        self.config.device = device.clone();
        self.config.firmware = self.config.selections.get(&device).cloned().or_else(|| {
            let info = self.registry.device(&device).ok()?.info();
            self.registry.loader(info.default_loader).ok()?.bundled(&self.root)
                .map(|files| FirmwareSelection { device: device.clone(), loader: None, files })
        });
        self.last_frame_texture = None;
        self.last_frame_status = None;
        self.status = self.selected_info().map_or_else(|| "Unknown phone profile.".into(),
            |info| format!("{} selected.", info.name));
        self.save_settings();
    }

    fn spawn_nand_file_dialog(&mut self) {
        if self.dialog_open { return; }
        let Some(paths) = self.config.firmware.as_ref() else { return; };
        let Some(info) = self.selected_info().filter(|info| info.has_nand) else { return; };
        self.dialog_open = true;
        let directory = paths.prepared_files(&self.registry).ok()
            .and_then(|files| files.companions.get("nand").cloned())
            .and_then(|path| path.parent().map(PathBuf::from))
            .unwrap_or_else(|| paths.files.image.parent().unwrap_or(&self.root).to_path_buf());
        let sender = self.events_tx.clone();
        std::thread::spawn(move || {
            let selection = rfd::FileDialog::new()
                .set_title(format!("Select {} physical NAND image", info.name))
                .add_filter("Physical NAND image", &["bin", "BIN", "img", "IMG", "nand", "NAND"])
                .add_filter("All files", &["*"])
                .set_directory(directory).pick_file();
            let _ = sender.send(FileDialogEvent::Nand(selection));
        });
        self.status = info.storage_hint.unwrap_or("Select a physical NAND image.").into();
    }

    fn drain_events(&mut self, ctx: &egui::Context) {
        if self.stopping.as_ref().is_some_and(|handle| handle.is_finished()) {
            let result = self.stopping.take().unwrap().join()
                .unwrap_or_else(|_| Some(Err("Could not stop the previous phone session.".into())));
            let pending = self.pending_run.take();
            match result {
                Some(Err(error)) => { self.status = error; }
                result => {
                    if let Some(paths) = pending { self.spawn_profile_run(paths); }
                    else if let Some(Ok(summary)) = result { self.status = summary; }
                }
            }
        }
        while let Ok(event) = self.events_rx.try_recv() {
            self.dialog_open = false;
            match event {
                FileDialogEvent::Firmware(Ok(mut paths)) => {
                    // Re-selecting the same image keeps its chosen NAND; a new
                    // firmware set starts with its own automatic sibling lookup.
                    if let Some(previous) = self.config.firmware.as_ref() {
                        if previous.device == paths.device && previous.files.image == paths.files.image
                            && !paths.files.companions.contains_key("nand") {
                            if let Some(nand) = previous.files.companions.get("nand") {
                                paths.files.companions.insert("nand".into(), nand.clone());
                            }
                        }
                    }
                    self.spawn_profile_run(paths);
                }
                FileDialogEvent::Firmware(Err(error)) if error == "cancelled" =>
                    self.status = "Firmware selection cancelled.".into(),
                FileDialogEvent::Firmware(Err(error)) =>
                    self.status = format!("Firmware selection failed: {error}"),
                FileDialogEvent::Nand(Some(path)) => {
                    let Some(mut selected) = self.config.firmware.clone() else { continue; };
                    selected.files.companions.insert("nand".into(), path);
                    if let Err(error) = selected.prepared_files(&self.registry) {
                        self.status = error.to_string();
                        continue;
                    }
                    self.stop();
                    self.config.firmware = Some(selected);
                    self.status = "NAND selected. Press Run to restart from reset; the source image is read-only.".into();
                    self.save_settings();
                    self.screen = Screen::Library;
                }
                FileDialogEvent::Nand(None) => self.status = "NAND selection cancelled.".into(),
            }
        }
        let mut latest = None;
        let mut finished = None;
        if let Some(worker) = self.worker.as_ref() {
            for _ in 0..10 {
                let Some(packet) = worker.audio.take() else { break; };
                if let Some(output) = self.audio_output.as_ref() { output.enqueue(packet); }
            }
            while let Ok(event) = worker.events.try_recv() {
                match event {
                    WorkerEvent::Frame(frame) => latest = Some(frame),
                    WorkerEvent::Lcd(frame) => latest = Some(e830::devices::runtime::Frame {
                        width:frame.width, height:frame.height, revision:frame.revision, rgb:frame.rgb() }),
                    WorkerEvent::Status(summary) => self.status = summary,
                    WorkerEvent::InputApplied(summary) => {
                        self.status = format!("Input: {summary}");
                        self.last_frame_status = Some(summary);
                    }
                    WorkerEvent::Finished(result) => finished = Some(result.unwrap_or_else(|error| error)),
                }
            }
        }
        if let Some(frame) = latest {
            if let Err(error) = frame.validate() {
                if let Some(worker) = &self.worker { worker.diagnostics.log("error", "desktop-render", error.to_string()); }
                self.stop(); self.status = error.to_string(); return;
            }
            let image = egui::ColorImage::from_rgb([frame.width, frame.height], &frame.rgb);
            if let Some(texture) = self.last_frame_texture.as_mut() {
                texture.set(image, egui::TextureOptions::NEAREST);
            } else {
                self.last_frame_texture = Some(ctx.load_texture("phone-fb", image, egui::TextureOptions::NEAREST));
            }
            self.frame_stats.record_frame();
        }
        if let Some(summary) = finished {
            self.stop();
            self.status = summary.clone();
            self.last_frame_status = Some(summary);
        }
    }

    fn ui_top_bar(&mut self, ui: &mut egui::Ui) {
        if self.brand_logo.is_none() {
            let icon = crate::branding::icon();
            let image = egui::ColorImage::from_rgba_unmultiplied([icon.width as usize, icon.height as usize], &icon.rgba);
            self.brand_logo = Some(ui.ctx().load_texture("ph0nix-logo", image, egui::TextureOptions::NEAREST));
        }
        ui.horizontal(|ui| {
            if let Some(logo) = &self.brand_logo {
                ui.image((logo.id(), egui::vec2(32.0, 32.0)));
            }
            ui.heading("ph0nix");
            ui.label(RichText::new("Phone firmware emulator").small().color(Color32::from_gray(160)));
            ui.add_space(16.0);
            if ui.selectable_label(self.screen == Screen::Library, "Library").clicked() { self.screen = Screen::Library; }
            if ui.selectable_label(self.screen == Screen::Devices, "Devices").clicked() { self.screen = Screen::Devices; }
            if ui.selectable_label(self.screen == Screen::Settings, "Settings").clicked()
                && self.screen != Screen::Settings {
                self.config_draft = Some(self.config.clone());
                self.screen = Screen::Settings;
            }
            ui.menu_button("Files", |ui| {
                if ui.button("Phone files").clicked() { self.files.open_phone(); ui.close_menu(); }
                if ui.button("Card images").clicked() { self.files.open_cards(); ui.close_menu(); }
                if ui.button("Bluetooth transfers").clicked() { self.files.open_bluetooth(); ui.close_menu(); }
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.add_enabled(!self.dialog_open, egui::Button::new("+ Import firmware")).clicked() {
                    self.spawn_firmware_file_dialog();
                }
            });
        });
        ui.separator();
    }

    fn ui_library(&mut self, ui: &mut egui::Ui) {
        let mut device = self.config.device.clone();
        ui.add_enabled_ui(!self.dialog_open, |ui| {
            ui.horizontal(|ui| {
                ui.label("Phone");
                egui::ComboBox::from_id_source("phone_profile")
                    .selected_text(self.selected_info().map_or(self.config.device.as_str(), |info| info.name))
                    .show_ui(ui, |ui| {
                        for info in self.registry.devices() {
                            ui.selectable_value(&mut device, info.id.into(), info.name);
                        }
                    });
            });
        });
        if device != self.config.device { self.select_device(device); }
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut self.search).hint_text("Search firmware"));
            if ui.selectable_value(&mut self.config.grid, true, "Grid").changed()
                || ui.selectable_value(&mut self.config.grid, false, "List").changed() { self.save_settings(); }
            if ui.checkbox(&mut self.config.sort_by_device, "Sort by phone").changed() { self.save_settings(); }
        });
        ui.add_space(8.0);
        if self.config.library.is_empty() {
            ui.add_space(80.0);
            ui.vertical_centered(|ui| {
                ui.label(RichText::new("No firmware selected").heading().color(Color32::from_gray(160)));
                ui.add_space(8.0);
                if let Some(info) = self.selected_info() {
                    ui.label(self.registry.loader(info.default_loader).unwrap().selection_hint());
                }
                ui.add_space(20.0);
                if ui.button("Import firmware...").clicked() { self.spawn_firmware_file_dialog(); }
            });
            return;
        }
        let mut entries: Vec<_> = self.config.library.iter().filter(|paths| {
            let name = self.registry.device(&paths.device).map(|p| p.info().name).unwrap_or("Phone");
            format!("{name} {}", paths.files.image.display()).to_lowercase().contains(&self.search.to_lowercase())
        }).cloned().collect();
        entries.sort_by_key(|paths| format!("{}{}", if self.config.sort_by_device { paths.device.as_str() } else { "" }, paths.files.image.display()));
        let mut run = None;
        let mut remove = None;
        egui::ScrollArea::vertical().max_height(260.0).show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                for paths in entries {
                    let name = self.registry.device(&paths.device).map(|p| p.info().name).unwrap_or("Phone");
                    egui::Frame::group(ui.style()).show(ui, |ui| {
                        if self.config.grid { ui.set_width(216.0); } else { ui.set_width(ui.available_width().max(300.0)); }
                        ui.label(RichText::new(name).strong());
                        ui.label(paths.files.image.file_name().unwrap_or_default().to_string_lossy());
                        ui.horizontal(|ui| {
                            if ui.button("Run").clicked() { run = Some(paths.clone()); }
                            ui.menu_button("⋮", |ui| {
                                if ui.button("Run firmware").clicked() { run = Some(paths.clone()); ui.close_menu(); }
                                if ui.button("Remove from library").clicked() {
                                    remove = Some((paths.device.clone(), paths.files.image.clone())); ui.close_menu();
                                }
                            });
                        });
                    });
                }
            });
        });
        if let Some((device, image)) = remove {
            self.config.library.retain(|paths| paths.device != device || paths.files.image != image);
            self.save_settings();
        }
        if let Some(paths) = run { self.spawn_profile_run(paths); }
        let info = self.selected_info();
        ui.add_space(12.0);
        if info.is_some_and(|info| info.has_nand) {
        if let Some(paths) = self.config.firmware.as_ref() {
            match paths.prepared_files(&self.registry).ok().and_then(|files| files.companions.get("nand").cloned()) {
                Some(path) => { ui.label(format!("NAND: {}", path.display())); }
                None => { ui.label("NAND: no image (erased flash)"); }
            }
        }
        ui.horizontal(|ui| {
            if ui.add_enabled(!self.dialog_open, egui::Button::new("Select NAND...")).clicked() {
                self.spawn_nand_file_dialog();
            }
            let explicit = self.config.firmware.as_ref().is_some_and(|paths| paths.files.companions.contains_key("nand"));
            if ui.add_enabled(!self.dialog_open && explicit, egui::Button::new("Use automatic NAND")).clicked() {
                self.stop();
                if let Some(paths) = self.config.firmware.as_mut() { paths.files.companions.remove("nand"); }
                self.status = "Automatic NAND selected. Press Run to restart from reset.".into();
                self.save_settings();
            }
        });
        if let Some(hint) = info.and_then(|info| info.storage_hint) { ui.label(RichText::new(hint).small()); }
        }
        if self.running {
            ui.add_space(12.0);
            if ui.button("Show run output").clicked() { self.screen = Screen::Run; }
        }
    }

    fn ui_devices(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.heading("Available phone profiles");
            for info in self.registry.devices() {
                ui.add_space(8.0);
                ui.label(RichText::new(info.name).strong());
                ui.label(info.support);
                if ui.button(format!("Select {}", info.name)).clicked() {
                    self.select_device(info.id.into());
                    self.screen = Screen::Library;
                }
            }
            ui.add_space(18.0);
            ui.heading("Manufacturer roadmap");
            for manufacturer in k750_native::catalog::MANUFACTURERS {
                ui.add_space(8.0);
                ui.label(RichText::new(format!("{} · {}", manufacturer.name, manufacturer.status)).strong());
                ui.label(manufacturer.detail);
            }
        });
    }

    fn ui_settings(&mut self, ui: &mut egui::Ui) {
        ui.heading("Settings");
        ui.add_space(8.0);
        let Some(mut draft) = self.config_draft.take() else { return; };
        egui::Grid::new("settings_grid").num_columns(2).spacing(Vec2::new(12.0, 8.0)).show(ui, |ui| {
            ui.label("Show FPS overlay"); ui.checkbox(&mut draft.show_fps, ""); ui.end_row();
            ui.label("Sound"); ui.checkbox(&mut draft.sound, ""); ui.end_row();
            ui.label("Borderless fullscreen");
            if ui.checkbox(&mut draft.fullscreen, "").changed() {
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Fullscreen(draft.fullscreen));
            }
            ui.end_row();
            ui.label("Resume saved K750i session"); ui.checkbox(&mut draft.resume, ""); ui.end_row();
        });
        ui.label("Presentation targets 60 FPS; guest LCD updates depend on firmware and host speed.");
        ui.label("Disabling resume cold boots K750i with its saved internal storage and settings.");
        ui.label("Interface based on J2ME-Loader (Apache-2.0); see bundled THIRD_PARTY_NOTICES.md.");
        ui.hyperlink_to("J2ME-Loader", "https://github.com/nikita36078/J2ME-Loader");
        ui.add_space(12.0);
        let mut save = false;
        let mut cancel = false;
        ui.horizontal(|ui| { save = ui.button("Save").clicked(); cancel = ui.button("Cancel").clicked(); });
        if save {
            let sound_changed = self.config.sound != draft.sound;
            self.config = draft;
            if sound_changed && self.running {
                self.audio_output = None;
                self.audio_error = None;
                if self.config.sound {
                    let diagnostics = self.worker.as_ref().map(|worker| worker.diagnostics.clone()).unwrap_or_default();
                    match crate::audio::AudioOutput::new(diagnostics) {
                        Ok(output) => self.audio_output = Some(output),
                        Err(error) => {
                            if let Some(worker) = &self.worker { worker.diagnostics.log("error", "desktop-audio", error.to_string()); }
                            self.audio_error = Some(format!("Sound unavailable: {error}"));
                        }
                    }
                }
            }
            self.status = "Settings saved.".into();
            self.save_settings();
            self.screen = Screen::Library;
        } else if cancel {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Fullscreen(self.config.fullscreen));
            self.screen = Screen::Library;
        } else { self.config_draft = Some(draft); }
    }

    fn discard_settings_preview(&mut self, ctx: &egui::Context) {
        if self.screen != Screen::Settings {
            if let Some(draft) = self.config_draft.take() {
                if draft.fullscreen != self.config.fullscreen {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(self.config.fullscreen));
                }
            }
        }
    }

    fn ui_run(&mut self, ui: &mut egui::Ui) {
        ui.heading("Phone");
        ui.add_space(8.0);
        self.ui_runtime_mode(ui);
        if self.running { ui.label(format!("Running {}…", self.selected_info().map_or("phone", |info| info.name))); }
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui.button("Fullscreen (F11)").clicked() { toggle_fullscreen(ui.ctx()); }
            if ui.button("Phone files").clicked() { self.files.open_phone(); }
            if ui.button("Card images").clicked() { self.files.open_cards(); }
            if ui.button("Bluetooth").clicked() { self.files.open_bluetooth(); }
            if ui.button("Developer").clicked() { self.developer.open(); }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Back to library").clicked() { self.stop(); self.screen = Screen::Library; }
            });
        });
        ui.add_space(6.0);
        ui.horizontal_top(|ui| {
            self.ui_run_screen(ui);
            ui.add_space(12.0);
            self.ui_phone_keys(ui);
        });
        ui.add_space(8.0);
        if let Some(status) = self.last_frame_status.as_ref() {
            ui.label(RichText::new(status).small().color(Color32::from_gray(170)));
        }
    }

    fn send_phone_keys(&mut self, keys: u32) {
        if keys != self.sent_keys {
            if let Some(worker) = &self.worker { worker.set_phone_keys(keys); }
            self.sent_keys = keys;
        }
    }

    fn ui_runtime_mode(&self, ui: &mut egui::Ui) {
        let Ok(profile) = self.registry.device(&self.config.device) else { return; };
        let notice = profile.runtime_notice();
        ui.label(RichText::new(notice.label).small());
        let colour = if notice.experimental { Color32::from_rgb(240, 190, 100) }
            else { Color32::from_gray(170) };
        ui.label(RichText::new(notice.detail).small().color(colour));
    }

    fn ui_phone_keys(&mut self, ui: &mut egui::Ui) {
        let keys_available = self.selected_info().is_some_and(|info| info.has_keys);
        let keyboard = if self.developer.is_open() || self.files.is_open() { 0 } else { ui.input(phone_keyboard_mask) };
        let mut mouse = 0;
        let mut mouse_clicks = 0;
        ui.vertical(|ui| {
            ui.label("Phone keypad");
            ui.add_enabled_ui(self.running && keys_available, |ui| {
                egui::Grid::new("phone_keypad").spacing(Vec2::new(5.0, 5.0)).show(ui, |ui| {
                    for row in [
                        [("Left soft", PhoneKey::SoftLeft), ("Up", PhoneKey::Up), ("Right soft", PhoneKey::SoftRight)],
                        [("Left", PhoneKey::Left), ("OK", PhoneKey::Ok), ("Right", PhoneKey::Right)],
                        [(if self.config.device == "k750i" { "Back" } else { "Call" }, PhoneKey::Call), ("Down", PhoneKey::Down), (if self.config.device == "k750i" { "C" } else { "End" }, PhoneKey::End)],
                        [("1", PhoneKey::One), ("2 abc", PhoneKey::Two), ("3 def", PhoneKey::Three)],
                        [("4 ghi", PhoneKey::Four), ("5 jkl", PhoneKey::Five), ("6 mno", PhoneKey::Six)],
                        [("7 pqrs", PhoneKey::Seven), ("8 tuv", PhoneKey::Eight), ("9 wxyz", PhoneKey::Nine)],
                        [("*", PhoneKey::Star), ("0", PhoneKey::Zero), ("#", PhoneKey::Hash)],
                    ] {
                        for (label, key) in row {
                            let button = egui::Button::new(label).min_size(Vec2::new(72.0, 35.0))
                                .selected(self.sent_keys & key.bit() != 0);
                            let response = ui.add(button);
                            // A click is reported on the release frame. Treating
                            // `clicked_by` as a held contact delayed release by
                            // another GUI frame while the guest kept running.
                            if response.is_pointer_button_down_on() { mouse |= key.bit(); }
                            if response.clicked_by(egui::PointerButton::Primary) {
                                mouse_clicks |= key.bit();
                            }
                        }
                        ui.end_row();
                    }
                });
            });
            ui.add_space(8.0);
            if self.config.device == "k750i" {
                ui.horizontal_wrapped(|ui| {
                    for binding in crate::app::sony_extra_keys() {
                        let response = ui.button(binding.label);
                        if response.is_pointer_button_down_on() { mouse |= binding.mask; }
                        if response.clicked() { mouse_clicks |= binding.mask; }
                    }
                });
            }
            let back_keys = if self.config.device == "k750i" { "Back/C" } else { "Call/End" };
            ui.label(RichText::new(format!("Arrows: navigate | Enter: OK\nF1/F2: soft keys | F3/F4: {back_keys}\n0-9: digits | F5/F6: */#")).small());
            if !keys_available { ui.label(RichText::new("Key input is unavailable for this phone.").small()); }
        });
        let keys = effective_keys(self.running && keys_available, keyboard, mouse);
        // A fast mouse press and release can both arrive between repaints, so
        // `is_pointer_button_down_on` is never observed. Preserve that short
        // click as an explicit down/up pair. A normally observed held contact
        // has its bit in `sent_keys`, and therefore only takes the release path.
        let click_pulse = missed_click_pulse(self.sent_keys, keys, mouse_clicks);
        if click_pulse != 0 { self.send_phone_keys(keys | click_pulse); }
        self.send_phone_keys(keys);
    }

    fn ui_run_screen(&mut self, ui: &mut egui::Ui) {
        let render_started = Instant::now();
        let Some(texture) = self.last_frame_texture.clone() else {
            let size = self.selected_info().and_then(|info| info.display)
                .map_or(Vec2::new(352.0, 440.0), |display| Vec2::new(display.width as f32 * 2.0, display.height as f32 * 2.0));
            ui.allocate_ui(size, |ui| { ui.label("Waiting for the phone to draw its first screen. First boot can take several minutes."); });
            return;
        };
        let size = texture.size_vec2();
        let available = ui.available_size();
        let scale = 2.0_f32.min(available.x / size.x).min(available.y / size.y).max(0.1);
        let (rect, _) = ui.allocate_exact_size(size * scale, Sense::hover());
        ui.painter().image(texture.id(), rect, Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
        if self.running {
            self.frame_stats.record_presentation();
            if let Some(worker) = &self.worker { worker.diagnostics.record_presentation(); }
        }
        if self.config.show_fps {
            let overlay_rect = Rect::from_min_size(rect.min + Vec2::new(6.0, 6.0), Vec2::new(180.0, 24.0));
            ui.painter().rect_filled(overlay_rect, 4.0, Color32::from_black_alpha(190));
            ui.painter().text(overlay_rect.min + Vec2::new(6.0, 4.0), egui::Align2::LEFT_TOP,
                self.frame_stats.overlay_text(), egui::FontId::monospace(13.0), Color32::LIGHT_GREEN);
        }
        if let Some(worker) = &self.worker { worker.diagnostics.record_render(render_started.elapsed()); }
    }
}

fn toggle_fullscreen(ctx: &egui::Context) {
    let fullscreen = ctx.input(|input| input.viewport().fullscreen.unwrap_or(false));
    ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(!fullscreen));
}

impl eframe::App for PhoneLauncher {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if ctx.input(|input| input.key_pressed(egui::Key::F11)) { toggle_fullscreen(ctx); }
        self.drain_events(ctx);
        self.discard_settings_preview(ctx);
        egui::TopBottomPanel::top("top").show(ctx, |ui| self.ui_top_bar(ui));
        self.discard_settings_preview(ctx);
        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(&self.status).small());
                if let Some(error) = &self.audio_error { ui.label(RichText::new(error).small()); }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(RichText::new(format!("v{}", env!("CARGO_PKG_VERSION"))).small().color(Color32::from_gray(140)));
                });
            });
        });
        egui::CentralPanel::default().show(ctx, |ui| match self.screen {
            Screen::Library => self.ui_library(ui),
            Screen::Devices => self.ui_devices(ui),
            Screen::Settings => self.ui_settings(ui),
            Screen::Run => self.ui_run(ui),
        });
        self.developer.show(ctx, self.worker.as_ref().map(|worker| &worker.diagnostics));
        self.files.show(ctx, self.worker.as_ref(), &self.settings_dir);
        if self.screen != Screen::Run || !self.running {
            self.send_phone_keys(0);
        }
        if self.running && self.screen == Screen::Run {
            // The renderer already waits for display sync. egui's delayed
            // repaint starts after that wait and can skip every other refresh.
            ctx.request_repaint();
        } else if self.running || self.dialog_open || self.stopping.is_some()
            || self.files.is_open() || self.developer.is_open() {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
    }
}

impl Drop for PhoneLauncher {
    fn drop(&mut self) {
        self.stop();
        if let Some(handle) = self.stopping.take() { let _ = handle.join(); }
    }
}

fn sony_extra_keys() -> Vec<k750_native::keys::KeyBinding> { k750_native::keys::bindings("k750i").into_iter().skip(21).collect() }

fn phone_keyboard_mask(input: &egui::InputState) -> u32 {
    if !input.focused || input.modifiers.ctrl || input.modifiers.alt || input.modifiers.command { return 0; }
    use egui::Key;
    [
        (Key::ArrowUp, PhoneKey::Up), (Key::ArrowDown, PhoneKey::Down),
        (Key::ArrowLeft, PhoneKey::Left), (Key::ArrowRight, PhoneKey::Right),
        (Key::Enter, PhoneKey::Ok), (Key::F1, PhoneKey::SoftLeft),
        (Key::F2, PhoneKey::SoftRight), (Key::F3, PhoneKey::Call),
        (Key::F4, PhoneKey::End), (Key::F5, PhoneKey::Star), (Key::F6, PhoneKey::Hash),
        (Key::Num0, PhoneKey::Zero), (Key::Num1, PhoneKey::One),
        (Key::Num2, PhoneKey::Two), (Key::Num3, PhoneKey::Three),
        (Key::Num4, PhoneKey::Four), (Key::Num5, PhoneKey::Five),
        (Key::Num6, PhoneKey::Six), (Key::Num7, PhoneKey::Seven),
        (Key::Num8, PhoneKey::Eight), (Key::Num9, PhoneKey::Nine),
    ].into_iter().fold(0, |mask, (key, phone)| {
        mask | if input.key_down(key) || input.key_pressed(key) { phone.bit() } else { 0 }
    })
}

/// The virtual on-screen keypad must respond to pointer presses even when
/// the OS does not report keyboard focus. Physical keyboard events are
/// already focus-gated inside `phone_keyboard_mask`, so only `running`
/// gates the combined mask here.
fn effective_keys(running: bool, keyboard: u32, mouse: u32) -> u32 {
    if running { keyboard | mouse } else { 0 }
}

fn missed_click_pulse(previous: u32, current: u32, clicks: u32) -> u32 {
    clicks & !previous & !current
}

// The presentation statistics and overlay match the original desktop frontend.
#[derive(Default)]
struct FrameStats {
    displayed_frames: u64,
    fps: f32,
    window_started_at: Option<Instant>,
    window_frames: u32,
    last_frame_at: Option<Instant>,
    last_frame_ms: Option<f32>,
}

impl FrameStats {
    fn record_frame(&mut self) {
        let now = Instant::now();
        if let Some(previous) = self.last_frame_at { self.last_frame_ms = Some(now.duration_since(previous).as_secs_f32() * 1000.0); }
        self.last_frame_at = Some(now);
        self.displayed_frames = self.displayed_frames.saturating_add(1);
    }

    fn record_presentation(&mut self) {
        let now = Instant::now();
        self.window_frames = self.window_frames.saturating_add(1);
        let started_at = *self.window_started_at.get_or_insert(now);
        let elapsed = now.duration_since(started_at);
        if elapsed >= Duration::from_secs(1) {
            self.fps = self.window_frames as f32 / elapsed.as_secs_f32();
            self.window_frames = 0;
            self.window_started_at = Some(now);
        }
    }

    fn overlay_text(&self) -> String {
        format!("{:.1} FPS · {} updates", self.fps, self.displayed_frames)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn launcher() -> PhoneLauncher {
        let config: Settings = serde_json::from_str(r#"{
            "device":"k750i",
            "firmware":{"device":"k750i","files":{"image":"K750_R1DB001_MAIN_EU_1_CL_RED49.mbn",
                "companions":{"fs":"K750_R1DB001_FS_EMEA_1_RED49.fbn"}}}
        }"#).unwrap();
        PhoneLauncher::new(std::env::temp_dir(), config)
    }

    #[test]
    fn keyboard_contacts_include_short_taps_and_release_on_focus_loss() {
        let ctx = egui::Context::default();
        let event = |key, pressed| egui::Event::Key {
            key, physical_key: None, pressed, repeat: false, modifiers: Default::default(),
        };
        let mut raw = egui::RawInput::default();
        raw.events = vec![event(egui::Key::Num1, true), event(egui::Key::Num1, false),
            event(egui::Key::ArrowUp, true)];
        ctx.begin_frame(raw);
        assert_eq!(ctx.input(phone_keyboard_mask), PhoneKey::One.bit() | PhoneKey::Up.bit());
        let _ = ctx.end_frame();
        ctx.begin_frame(egui::RawInput::default());
        assert_eq!(ctx.input(phone_keyboard_mask), PhoneKey::Up.bit());
        let _ = ctx.end_frame();
        let mut raw = egui::RawInput::default();
        raw.focused = false;
        ctx.begin_frame(raw);
        assert_eq!(ctx.input(phone_keyboard_mask), 0);
        let _ = ctx.end_frame();
    }

    #[test]
    fn virtual_keypad_sends_keys_without_window_focus() {
        // Regression: the on-screen keypad previously required egui
        // `input.focused`, so in environments where winit never reports
        // focus (or while the window is unfocused) mouse presses were
        // translated to zero. Physical keyboard events are already
        // focus-gated; virtual buttons must not be.
        let ok = PhoneKey::Ok.bit();
        assert_eq!(effective_keys(true, 0, ok), ok);
        assert_eq!(effective_keys(false, 0, ok), 0);
        assert_eq!(effective_keys(true, PhoneKey::Up.bit(), ok), PhoneKey::Up.bit() | ok);
        // Unfocused physical keyboard still yields 0 from the mask.
        assert_eq!(effective_keys(true, 0, 0), 0);
    }

    #[test]
    fn fast_mouse_click_gets_a_pulse_but_normal_release_does_not_repeat() {
        let up = PhoneKey::Up.bit();
        assert_eq!(missed_click_pulse(0, 0, up), up);
        assert_eq!(missed_click_pulse(0, up, up), 0);
        assert_eq!(missed_click_pulse(up, 0, up), 0);
        assert_eq!(missed_click_pulse(0, 0, 0), 0);
    }

    #[test]
    fn runtime_mode_labels_do_not_claim_a_sim_menu_or_enable_hle() {
        let plain = e830::devices::e830::frontend::RuntimeMode::default();
        assert!(plain.label().contains("no SIM"));
        assert!(plain.label().contains("DSP boot HLE: off"));
        assert!(plain.notice().contains("DSP boot HLE is off"));
        let no_card = e830::devices::e830::frontend::RuntimeMode { boot_hle: true, ..plain };
        assert!(no_card.notice().contains("No-SIM standby"));
        assert!(no_card.notice().contains("arrows navigate it"));
        let candidate = e830::devices::e830::frontend::RuntimeMode { sim: true, rf_hle: true, battery_hle: true, ..no_card };
        assert!(candidate.label().contains("experimental SIM"));
        assert!(candidate.label().contains("empty air"));
        assert!(candidate.label().contains("3.7 V"));
        assert!(candidate.notice().contains("Experimental SIM boot is incomplete"));
        assert!(candidate.notice().contains("not a verified SIM standby or app menu"));
    }

    #[test]
    fn settings_round_trip_preserves_k750_firmware_and_sound_preference() {
        let app = launcher();
        let settings: Settings = serde_json::from_slice(&serde_json::to_vec(&app.config).unwrap()).unwrap();
        assert_eq!(settings.device, "k750i");
        assert!(settings.sound);
        assert_eq!(settings.firmware.unwrap().files.companions.get("fs").cloned(),
            Some("K750_R1DB001_FS_EMEA_1_RED49.fbn".into()));
    }

    #[test]
    fn library_migration_removes_failed_presets_and_preserves_imported_files() {
        struct Directory(PathBuf);
        impl Drop for Directory {
            fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); }
        }
        let directory = Directory(std::env::temp_dir().join(format!(
            "phone-profile-settings-{}-{}", std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        )));
        std::fs::create_dir(&directory.0).unwrap();
        let mut config = launcher().config.clone();
        let sony = config.firmware.clone().unwrap();
        let removed: Vec<_> = ["e830", "n500"].into_iter().map(|device| {
            let image = directory.0.join(format!("{device}.bin"));
            std::fs::write(&image, b"keep imported source").unwrap();
            FirmwareSelection { device: device.into(), loader: None,
                files: e830::firmware::FirmwareFiles { image, companions: Default::default() } }
        }).collect();
        config.library = vec![sony.clone(), removed[0].clone(), removed[1].clone()];
        config.selections.extend(removed.iter().cloned().map(|files| (files.device.clone(), files)));
        config.device = "e830".into();
        config.firmware = Some(removed[0].clone());
        let mut app = PhoneLauncher::new(directory.0.clone(), config);
        assert_eq!(app.config.device, "k750i");
        assert_eq!(app.config.library.len(), 1);
        assert_eq!(app.config.firmware.as_ref().unwrap().files.image, sony.files.image);
        assert!(app.config.selections.keys().all(|device| device == "k750i"));
        app.save_settings();
        let restored = Settings::load(&directory.0);
        assert_eq!(restored.device, "k750i");
        assert_eq!(restored.library.len(), 1);
        for selection in removed {
            assert_eq!(std::fs::read(selection.files.image).unwrap(), b"keep imported source");
        }
    }

    #[test]
    fn cancelled_firmware_dialog_keeps_existing_selection() {
        let mut app = launcher();
        app.dialog_open = true;
        let previous = app.config.firmware.as_ref().unwrap().files.image.clone();
        app.events_tx.send(FileDialogEvent::Firmware(Err("cancelled".into()))).unwrap();
        app.drain_events(&egui::Context::default());
        assert!(!app.dialog_open);
        assert_eq!(app.config.firmware.as_ref().unwrap().files.image, previous);
        assert_eq!(app.status, "Firmware selection cancelled.");
    }

    #[test]
    fn removed_device_selection_keeps_k750_selection() {
        let mut app = launcher();
        app.running = true;
        let previous = app.config.firmware.as_ref().unwrap().files.image.clone();
        app.select_device("n500".into());
        assert!(app.running);
        assert_eq!(app.config.firmware.as_ref().unwrap().files.image, previous);
        assert_eq!(app.config.device, "k750i");
    }
}
