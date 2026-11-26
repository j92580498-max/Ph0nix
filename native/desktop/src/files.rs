//! Phone files, raw card images and real host-adapter Bluetooth in native windows.
use std::{io::{self, Read, Write}, path::{Path, PathBuf}, sync::{mpsc::{self, Receiver, Sender}, Arc, atomic::{AtomicBool, Ordering}}, time::{Duration, Instant, SystemTime, UNIX_EPOCH}};
use db2010_host::{diagnostics::DiagnosticsHandle, storage::{CardImage, CardInfo, FileEntry, FileRequest, FileSnapshot, PhoneFiles},
    bluetooth::{OppClient, receive_one, ReceivedFile, platform::{RfcommListener, RfcommStream}}};
use eframe::egui::{self, RichText};
use k750_native::worker::Worker;

#[derive(Clone)]
enum Mode { Phone, Cards, Card(PathBuf), Bluetooth }
enum Event {
    Request(Mode, FileRequest),
    Card(Result<(PathBuf, CardInfo, Vec<String>, Vec<FileEntry>), String>),
    Created(Result<PathBuf, String>),
    Bluetooth(Result<Option<ReceivedFile>, String>),
    SendFile(PathBuf),
}

pub struct FileTools {
    open: bool,
    mode: Mode,
    phone: Option<PhoneFiles>,
    diagnostics: Option<DiagnosticsHandle>,
    snapshot: FileSnapshot,
    card_info: Option<CardInfo>,
    boot_card: Option<PathBuf>,
    due: Instant,
    status: String,
    busy: bool,
    move_sources: bool,
    folder: String,
    copy_source: Option<Vec<String>>,
    copy_destination: String,
    copy_move: bool,
    card_name: String,
    card_size: u64,
    tx: Sender<Event>,
    rx: Receiver<Event>,
    awaiting: Option<(u64, Option<PathBuf>, bool)>,
    address: String,
    channel: u8,
    explicit_channel: bool,
    bt_source: Option<PathBuf>,
    bt_busy: bool,
    bt_cancel: Arc<AtomicBool>,
    inbox: Vec<ReceivedFile>,
    root: PathBuf,
    inbox_loaded: bool,
}

impl Default for FileTools {
    fn default() -> Self {
        let (tx, rx) = mpsc::channel();
        Self { open: false, mode: Mode::Phone, phone: None, diagnostics: None, snapshot: FileSnapshot::default(),
            card_info: None, boot_card: None, due: Instant::now(), status: String::new(), busy: false, move_sources: false,
            folder: String::new(), copy_source: None, copy_destination: String::new(), copy_move: false,
            card_name: "ph0nix-card".into(), card_size: 64, tx, rx, awaiting: None, address: String::new(),
            channel: 12, explicit_channel: false, bt_source: None, bt_busy: false, bt_cancel: Arc::new(AtomicBool::new(false)),
            inbox: Vec::new(), root: PathBuf::new(), inbox_loaded: false }
    }
}

impl FileTools {
    pub fn is_open(&self) -> bool { self.open }
    pub fn boot_card(&self) -> Option<&Path> { self.boot_card.as_deref() }
    pub fn open_phone(&mut self) { self.open = true; self.mode = Mode::Phone; self.due = Instant::now(); self.list(Vec::new()); }
    pub fn open_cards(&mut self) { self.open = true; self.mode = Mode::Cards; }
    pub fn open_bluetooth(&mut self) { self.open = true; self.mode = Mode::Bluetooth; }

    pub fn show(&mut self, ctx: &egui::Context, worker: Option<&Worker>, root: &Path) {
        self.root = root.to_owned();
        if !self.inbox_loaded {
            self.inbox_loaded = true;
            if let Ok(entries) = std::fs::read_dir(root.join("bluetooth-inbox")) {
                self.inbox = entries.flatten().map(|entry| entry.path()).filter(|path| path.extension().is_some_and(|ext| ext == "json"))
                    .filter_map(|path| std::fs::read(path).ok().and_then(|bytes| serde_json::from_slice::<ReceivedFile>(&bytes).ok()))
                    .filter(|file| file.path.is_file()).collect();
            }
        }
        self.phone = worker.map(|worker| worker.files.clone());
        self.diagnostics = worker.map(|worker| worker.diagnostics.clone()).or(self.diagnostics.take());
        while let Ok(event) = self.rx.try_recv() {
            match event {
                Event::Request(mode, request) => { self.mode = mode; self.request(request, None); },
                Event::Card(result) => {
                    self.busy = false;
                    match result {
                        Ok((image, info, path, entries)) => {
                            if matches!(&self.mode, Mode::Card(current) if *current == image) {
                                self.card_info = Some(info); self.snapshot.path = path; self.snapshot.entries = entries;
                            }
                            self.status = "Card operation complete.".into();
                        },
                        Err(error) => self.error(error),
                    }
                },
                Event::Created(result) => { self.busy = false; match result {
                    Ok(image) => { self.mode = Mode::Card(image); self.list(Vec::new()); },
                    Err(error) => self.error(error),
                } },
                Event::SendFile(path) => { self.bt_source = Some(path); self.mode = Mode::Bluetooth; },
                Event::Bluetooth(result) => {
                    self.bt_busy = false;
                    match result {
                        Ok(Some(file)) => { self.status = format!("Received {} ({} bytes). Import it below.", file.name, file.bytes); self.inbox.push(file); },
                        Ok(None) => self.status = "Bluetooth transfer complete.".into(),
                        Err(error) => self.error(error),
                    }
                }
            }
        }
        if Instant::now() >= self.due {
            self.due = Instant::now() + Duration::from_millis(250);
            let mut refresh_path = None;
            if let Some(phone) = &self.phone {
                let snapshot = phone.snapshot();
                if let Some((id, send, refresh)) = &self.awaiting {
                    if snapshot.job.as_ref().is_some_and(|job| job.id == *id) && !snapshot.busy {
                        let send = send.clone(); let refresh = *refresh; self.awaiting = None;
                        if let Some(error) = snapshot.job.as_ref().and_then(|job| job.error.clone()) { self.error(error); }
                        else if snapshot.job.as_ref().is_some_and(|job| job.phase == "complete") {
                            self.status = "Phone operation complete.".into();
                            if let Some(path) = send { self.bt_source = Some(path); self.mode = Mode::Bluetooth; }
                            else if refresh && self.open && matches!(self.mode, Mode::Phone) { refresh_path = Some(snapshot.path.clone()); }
                        }
                        else { self.status = "Phone operation interrupted. The source was kept.".into(); }
                    }
                }
                if matches!(self.mode, Mode::Phone) { self.snapshot = snapshot; }
            } else if self.awaiting.take().is_some() { self.status = "Phone stopped during the file operation. Retry after it resumes.".into(); }
            if let Some(path) = refresh_path { self.list(path); }
        }
        if !self.open { return; }
        let mut open = self.open;
        egui::Window::new("ph0nix files and transfers").open(&mut open).default_size([640.0, 460.0]).show(ctx, |ui| {
            ui.horizontal(|ui| {
                if ui.selectable_label(matches!(self.mode, Mode::Phone), "Phone files").clicked() { self.mode = Mode::Phone; self.list(Vec::new()); }
                if ui.selectable_label(matches!(self.mode, Mode::Cards | Mode::Card(_)), "Card images").clicked() { self.mode = Mode::Cards; }
                if ui.selectable_label(matches!(self.mode, Mode::Bluetooth), "Bluetooth").clicked() { self.mode = Mode::Bluetooth; }
            });
            ui.separator();
            match self.mode {
                Mode::Cards => self.cards(ui),
                Mode::Bluetooth => self.bluetooth(ui),
                _ => self.browser(ui, worker),
            }
            ui.separator(); ui.label(&self.status);
        });
        self.open = open;
    }

    fn error(&mut self, message: String) {
        if let Some(diagnostics) = &self.diagnostics { diagnostics.log("error", "files", &message); }
        self.status = message;
    }

    fn list(&mut self, path: Vec<String>) { self.request(FileRequest::List { path }, None); }

    fn request(&mut self, request: FileRequest, send: Option<PathBuf>) {
        let refresh = matches!(&request, FileRequest::Import { .. } | FileRequest::Copy { .. } |
            FileRequest::MakeDirectory { .. } | FileRequest::Delete { .. } | FileRequest::DeleteVerified { .. });
        match &self.mode {
            Mode::Phone => {
                let result = self.phone.as_ref().ok_or_else(|| anyhow::anyhow!("Start the phone before browsing its files")).and_then(|files| files.request(request));
                match result { Ok(id) => { self.awaiting = Some((id, send, refresh)); self.status = "Working…".into(); }, Err(error) => self.error(error.to_string()) }
            },
            Mode::Card(image) => {
                if self.busy { return; }
                self.busy = true; self.status = "Working…".into();
                let image = image.clone(); let tx = self.tx.clone();
                std::thread::spawn(move || {
                    let result = card_request(&image, request).map_err(|error| format!("{error:#}"));
                    let succeeded = result.is_ok();
                    let _ = tx.send(Event::Card(result));
                    if succeeded { if let Some(path) = send { let _ = tx.send(Event::SendFile(path)); } }
                });
            },
            _ => {},
        }
    }

    fn browser(&mut self, ui: &mut egui::Ui, worker: Option<&Worker>) {
        if let Mode::Card(image) = self.mode.clone() {
            ui.label(image.file_name().unwrap_or_default().to_string_lossy());
            if let Some(info) = &self.card_info { ui.label(format!("FAT16 · {:.1} MiB free", info.free_bytes as f64 / 1048576.0)); }
            if let Some(inserted) = worker.and_then(Worker::inserted_memory_stick) {
                if inserted == image.as_path() {
                    ui.label("Memory Stick inserted. Stop the phone to change it.");
                    if ui.button("Browse in Phone files").clicked() { self.open_phone(); }
                    return;
                }
            }
            if worker.is_none() && !self.busy && self.card_info.is_some() {
                if self.boot_card.as_ref() == Some(&image) {
                    ui.label("Selected for the next K750i boot.");
                    if ui.button("Start without a Memory Stick").clicked() { self.boot_card = None; }
                } else if ui.button("Use for the next K750i boot").clicked() {
                    self.boot_card = Some(image);
                }
            }
        }
        ui.monospace(format!("/{}", self.snapshot.path.join("/")));
        let path = self.snapshot.path.clone();
        let busy = self.busy || (matches!(self.mode, Mode::Phone) && self.snapshot.busy);
        ui.add_enabled_ui(!busy, |ui| {
            ui.horizontal(|ui| {
                if ui.button("Up").clicked() && !path.is_empty() { self.list(path[..path.len()-1].to_vec()); }
                if ui.button("Refresh").clicked() { self.list(path.clone()); }
                if ui.button("Import…").clicked() { self.import_dialog(path.clone()); }
                ui.checkbox(&mut self.move_sources, "Move imported/exported sources");
            });
            ui.horizontal(|ui| {
                ui.label("New folder:"); ui.text_edit_singleline(&mut self.folder);
                if ui.button("Create").clicked() && !self.folder.trim().is_empty() {
                    let mut child = path.clone(); child.push(self.folder.trim().to_owned());
                    self.request(FileRequest::MakeDirectory { path: child }, None); self.folder.clear();
                }
            });
            egui::ScrollArea::vertical().max_height(275.0).show(ui, |ui| {
                for entry in self.snapshot.entries.clone() {
                    let mut source = path.clone(); source.push(entry.name.clone());
                    ui.horizontal(|ui| {
                        if ui.selectable_label(false, if entry.directory { format!("{}/", entry.name) } else { entry.name.clone() }).clicked() && entry.directory { self.list(source.clone()); }
                        if let Some(size) = entry.size { ui.label(RichText::new(format!("{size} bytes")).small()); }
                        if !entry.directory {
                            if ui.button("Export…").clicked() { self.export_dialog(source.clone()); }
                            if ui.button("Copy/move…").clicked() { self.copy_source = Some(source.clone()); self.copy_destination = format!("/{}", source.join("/")); }
                            if ui.button("Bluetooth…").clicked() {
                                let host = self.root.join("transfer-staging").join(format!("{}-{}", std::process::id(), SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos())).join(&entry.name);
                                self.request(FileRequest::Export { path: source.clone(), host: host.clone(), move_source: false }, Some(host));
                            }
                        }
                    });
                }
                if self.snapshot.entries.is_empty() { ui.label("No entries. Refresh to connect or open a folder."); }
            });
            if let Some(source) = self.copy_source.clone() {
                ui.separator(); ui.label(format!("Copy {} to:", source.last().unwrap_or(&String::new())));
                ui.text_edit_singleline(&mut self.copy_destination); ui.checkbox(&mut self.copy_move, "Move source");
                ui.horizontal(|ui| {
                    if ui.button("Apply").clicked() {
                        let destination = self.copy_destination.split('/').filter(|s| !s.is_empty()).map(str::to_owned).collect();
                        self.request(FileRequest::Copy { source, destination, replace: false, move_source: self.copy_move }, None); self.copy_source = None;
                    }
                    if ui.button("Cancel").clicked() { self.copy_source = None; }
                });
            }
        });
        if matches!(self.mode, Mode::Phone) {
            if let Some(job) = &self.snapshot.job { if self.snapshot.busy { ui.label(format!("{}: {} ({} bytes)", job.operation, job.phase, job.bytes)); } }
        }
    }

    fn import_dialog(&self, path: Vec<String>) {
        let tx = self.tx.clone(); let mode = self.mode.clone(); let move_source = self.move_sources;
        std::thread::spawn(move || {
            if let Some(host) = rfd::FileDialog::new().set_title("Import into phone/card folder").pick_file() {
                if let Some(name) = host.file_name().and_then(|name| name.to_str()) {
                    let mut path = path; path.push(name.to_owned());
                    let _ = tx.send(Event::Request(mode, FileRequest::Import { path, host, replace: false, move_source }));
                }
            }
        });
    }
    fn export_dialog(&self, path: Vec<String>) {
        let tx = self.tx.clone(); let mode = self.mode.clone(); let move_source = self.move_sources;
        std::thread::spawn(move || {
            let Some(name) = path.last() else { return; };
            if let Some(host) = rfd::FileDialog::new().set_file_name(name).set_title("Export phone/card file").save_file() {
                let _ = tx.send(Event::Request(mode, FileRequest::Export { path, host, move_source }));
            }
        });
    }

    fn cards(&mut self, ui: &mut egui::Ui) {
        ui.label("Raw FAT16 images, 16–1024 MiB.");
        ui.label("Open an image and select it before starting K750i.");
        if let Some(image) = &self.boot_card {
            ui.label(format!("Selected: {}", image.display()));
            if ui.button("Clear selection").clicked() { self.boot_card = None; }
        }
        ui.horizontal(|ui| {
            ui.text_edit_singleline(&mut self.card_name);
            ui.add(egui::DragValue::new(&mut self.card_size).clamp_range(16..=1024).suffix(" MiB"));
            if ui.add_enabled(!self.busy, egui::Button::new("Create image")).clicked() {
                if self.card_name.is_empty() || self.card_name.contains(['/', '\\']) || matches!(self.card_name.as_str(), "." | "..") { self.error("Choose a valid image name".into()); }
                else {
                    self.busy = true; let image = self.root.join("cards").join(format!("{}.img", self.card_name));
                    let size = self.card_size; let tx = self.tx.clone();
                    std::thread::spawn(move || { let result = CardImage::create(&image, size).map(|_| image).map_err(|e| e.to_string()); let _ = tx.send(Event::Created(result)); });
                }
            }
            if ui.button("Open image…").clicked() {
                let tx = self.tx.clone();
                std::thread::spawn(move || { if let Some(image) = rfd::FileDialog::new().add_filter("Raw FAT16 image", &["img"]).pick_file() { let _ = tx.send(Event::Created(Ok(image))); } });
            }
        });
        if let Ok(entries) = std::fs::read_dir(self.root.join("cards")) {
            for image in entries.flatten().map(|entry| entry.path()).filter(|path| path.extension().is_some_and(|ext| ext == "img")) {
                if ui.button(image.file_name().unwrap_or_default().to_string_lossy()).clicked() { self.mode = Mode::Card(image); self.list(Vec::new()); }
            }
        }
    }

    fn bluetooth(&mut self, ui: &mut egui::Ui) {
        ui.label("Host Bluetooth bridge. Pair devices and enable Bluetooth in your operating system first.");
        ui.label(RichText::new("Receiving advertises the standard Object Push service. Linux requires BlueZ and libbluetooth; Windows uses Winsock Bluetooth.").small());
        ui.horizontal(|ui| { ui.label("Peer address:"); ui.text_edit_singleline(&mut self.address); });
        ui.horizontal(|ui| {
            ui.checkbox(&mut self.explicit_channel, "Use a specific RFCOMM channel");
            ui.add(egui::DragValue::new(&mut self.channel).clamp_range(1..=30));
        });
        if let Some(source) = &self.bt_source { ui.label(format!("Send: {}", source.display())); }
        ui.add_enabled_ui(!self.bt_busy, |ui| {
            ui.horizontal(|ui| {
                if ui.button("Choose host file…").clicked() {
                    let tx = self.tx.clone(); std::thread::spawn(move || { if let Some(file) = rfd::FileDialog::new().pick_file() { let _ = tx.send(Event::SendFile(file)); } });
                }
                if ui.add_enabled(self.bt_source.is_some(), egui::Button::new("Send")).clicked() { self.send_bluetooth(); }
                if ui.button("Receive (120 s)").clicked() { self.receive_bluetooth(); }
            });
        });
        if self.bt_busy && ui.button("Cancel transfer").clicked() { self.bt_cancel.store(true, Ordering::Relaxed); self.status = "Cancelling Bluetooth transfer…".into(); }
        ui.separator();
        for file in self.inbox.clone() {
            ui.horizontal(|ui| {
                ui.label(format!("{} · {} bytes", file.name, file.bytes));
                if ui.button("Import into phone/Other").clicked() {
                    self.mode = Mode::Phone;
                    self.request(FileRequest::Import { path: vec!["Phone memory".into(), "Other".into(), file.name.clone()], host: file.path.clone(), replace: false, move_source: false }, None);
                }
            });
        }
    }

    fn send_bluetooth(&mut self) {
        let Some(file) = self.bt_source.clone() else { return; };
        let address = self.address.clone(); let channel = self.explicit_channel.then_some(self.channel);
        let tx = self.tx.clone(); let diagnostics = self.diagnostics.clone();
        self.bt_busy = true; self.status = "Connecting Bluetooth…".into(); self.bt_cancel.store(false, Ordering::Relaxed);
        let cancelled = self.bt_cancel.clone();
        std::thread::spawn(move || {
            let result = (|| -> anyhow::Result<Option<ReceivedFile>> {
                let stream = RfcommStream::connect(&address, channel, Duration::from_secs(10))?;
                let mut client = OppClient::new(Cancellable { stream, cancelled: cancelled.clone() }); client.connect()?;
                anyhow::ensure!(!cancelled.load(Ordering::Relaxed), "Bluetooth transfer cancelled");
                let bytes = client.send_file(&file)?; client.disconnect()?;
                if let Some(diagnostics) = diagnostics { diagnostics.log("info", "bluetooth", format!("Sent {}: {bytes} bytes", file.display())); }
                Ok(None)
            })().map_err(|error| format!("{error:#}"));
            let _ = tx.send(Event::Bluetooth(result));
        });
    }

    fn receive_bluetooth(&mut self) {
        let tx = self.tx.clone(); let directory = self.root.join("bluetooth-inbox"); let channel = self.channel;
        self.bt_busy = true; self.status = "Waiting for a Bluetooth file (120 s)…".into(); self.bt_cancel.store(false, Ordering::Relaxed);
        let cancelled = self.bt_cancel.clone();
        let diagnostics = self.diagnostics.clone();
        std::thread::spawn(move || {
            let result = (|| -> anyhow::Result<Option<ReceivedFile>> {
                let listener = RfcommListener::bind(channel)?;
                let deadline = Instant::now() + Duration::from_secs(120);
                let mut stream = loop {
                    anyhow::ensure!(!cancelled.load(Ordering::Relaxed), "Bluetooth receive cancelled");
                    anyhow::ensure!(Instant::now() < deadline, "Bluetooth receive timed out");
                    match listener.accept(Duration::from_millis(500)) { Ok((stream, _)) => break stream, Err(error) => {
                        if !error.to_string().to_lowercase().contains("tim") { return Err(error); }
                    } }
                };
                let mut stream = Cancellable { stream: &mut stream, cancelled: cancelled.clone() };
                let file = receive_one(&mut stream, &directory)?;
                let metadata = file.path.with_extension("json");
                if let Err(error) = std::fs::write(metadata, serde_json::to_vec(&file)?) {
                    if let Some(diagnostics) = &diagnostics { diagnostics.log("error", "bluetooth", format!("Received file saved, but receipt persistence failed: {error}")); }
                }
                Ok(Some(file))
            })().map_err(|error| format!("{error:#}"));
            let _ = tx.send(Event::Bluetooth(result));
        });
    }
}

fn card_request(image: &Path, request: FileRequest) -> anyhow::Result<(PathBuf, CardInfo, Vec<String>, Vec<FileEntry>)> {
    let card = CardImage::open(image)?;
    let path = match request {
        FileRequest::List { path } => path,
        FileRequest::Import { path, host, replace, move_source } => { card.import_file(&path, &host, replace, move_source)?; path[..path.len()-1].to_vec() },
        FileRequest::Export { path, host, move_source } => { card.export_file(&path, &host, move_source)?; path[..path.len()-1].to_vec() },
        FileRequest::Copy { source, destination, replace, move_source } => { card.copy(&source, &destination, replace, move_source)?; destination[..destination.len()-1].to_vec() },
        FileRequest::MakeDirectory { path } => { card.make_directory(&path)?; path[..path.len()-1].to_vec() },
        FileRequest::Delete { path } => { card.delete(&path)?; path[..path.len()-1].to_vec() },
        FileRequest::DeleteVerified { path, bytes, sha256 } => { card.delete_verified(&path, bytes, &sha256)?; path[..path.len()-1].to_vec() },
        FileRequest::Disconnect => Vec::new(),
    };
    let entries = card.list(&path)?; let info = card.info()?;
    Ok((image.to_owned(), info, path, entries))
}

impl Drop for FileTools { fn drop(&mut self) { self.bt_cancel.store(true, Ordering::Relaxed); } }

struct Cancellable<T> { stream: T, cancelled: Arc<AtomicBool> }
impl<T: Read> Read for Cancellable<T> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        if self.cancelled.load(Ordering::Relaxed) { return Err(io::Error::new(io::ErrorKind::ConnectionAborted, "Bluetooth transfer cancelled")); }
        self.stream.read(bytes)
    }
}
impl<T: Write> Write for Cancellable<T> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.cancelled.load(Ordering::Relaxed) { return Err(io::Error::new(io::ErrorKind::ConnectionAborted, "Bluetooth transfer cancelled")); }
        self.stream.write(bytes)
    }
    fn flush(&mut self) -> io::Result<()> { self.stream.flush() }
}
