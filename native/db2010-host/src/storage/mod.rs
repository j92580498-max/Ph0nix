// SPDX-License-Identifier: GPL-2.0-or-later
//! Live phone file operations and portable removable FAT images.
pub mod card;
pub mod obex;
pub use card::{CardImage, CardInfo};

use anyhow::{bail, ensure, Context, Result};
use obex::{validate_path, ObexClient};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    net::{Shutdown, TcpListener, TcpStream},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, SyncSender},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

pub const MAX_FILE_BYTES: usize = 32 * 1024 * 1024;
static NONCE: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileEntry {
    pub name: String,
    pub directory: bool,
    pub size: Option<u64>,
    pub modified: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum FileRequest {
    List {
        path: Vec<String>,
    },
    Import {
        path: Vec<String>,
        host: PathBuf,
        #[serde(default)]
        replace: bool,
        #[serde(default)]
        move_source: bool,
    },
    ImportInternal {
        name: String,
        host: PathBuf,
    },
    Export {
        path: Vec<String>,
        host: PathBuf,
        #[serde(default)]
        move_source: bool,
    },
    Copy {
        source: Vec<String>,
        destination: Vec<String>,
        #[serde(default)]
        replace: bool,
        #[serde(default)]
        move_source: bool,
    },
    MakeDirectory {
        path: Vec<String>,
    },
    Delete {
        path: Vec<String>,
    },
    DeleteVerified {
        path: Vec<String>,
        bytes: u64,
        sha256: String,
    },
    Disconnect,
}
impl FileRequest {
    fn operation(&self) -> &'static str {
        match self {
            Self::List { .. } => "list",
            Self::Import { .. } | Self::ImportInternal { .. } => "import",
            Self::Export { .. } => "export",
            Self::Copy {
                move_source: true, ..
            } => "move",
            Self::Copy { .. } => "copy",
            Self::MakeDirectory { .. } => "make_directory",
            Self::Delete { .. } => "delete",
            Self::DeleteVerified { .. } => "delete_verified",
            Self::Disconnect => "disconnect",
        }
    }
    fn validate(&self) -> Result<()> {
        match self {
            Self::List { path } => validate_path(path),
            Self::Import { path, host, .. } | Self::Export { path, host, .. } => {
                validate_path(path)?;
                ensure!(!path.is_empty(), "File name required");
                ensure!(!host.as_os_str().is_empty(), "Host file path required");
                Ok(())
            }
            Self::ImportInternal { name, host } => {
                validate_path(std::slice::from_ref(name))?;
                ensure!(!host.as_os_str().is_empty(), "Host file path required");
                Ok(())
            }
            Self::Copy {
                source,
                destination,
                ..
            } => {
                validate_path(source)?;
                validate_path(destination)?;
                ensure!(
                    !source.is_empty()
                        && !destination.is_empty()
                        && source.iter().map(|p| p.to_lowercase()).collect::<Vec<_>>()
                            != destination
                                .iter()
                                .map(|p| p.to_lowercase())
                                .collect::<Vec<_>>(),
                    "Choose different source and destination files"
                );
                Ok(())
            }
            Self::MakeDirectory { path } | Self::Delete { path } => {
                validate_path(path)?;
                ensure!(!path.is_empty(), "File or folder name required");
                Ok(())
            }
            Self::DeleteVerified {
                path,
                bytes,
                sha256,
            } => {
                validate_path(path)?;
                ensure!(!path.is_empty(), "File name required");
                expected_digest(*bytes, sha256)?;
                Ok(())
            }
            Self::Disconnect => Ok(()),
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileJob {
    pub id: u64,
    pub operation: String,
    pub phase: String,
    pub bytes: u64,
    pub total: Option<u64>,
    pub error: Option<String>,
    pub host: Option<PathBuf>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct FileSnapshot {
    pub attached: bool,
    pub serial_ready: bool,
    pub connected: bool,
    pub busy: bool,
    pub path: Vec<String>,
    pub entries: Vec<FileEntry>,
    pub job: Option<FileJob>,
}
enum Command {
    Request(u64, FileRequest),
    Quiesce(SyncSender<std::result::Result<(), String>>),
}
struct PendingImport {
    id: u64,
    source: PathBuf,
    expected: (u64, [u8; 32]),
}
struct Inner {
    state: Mutex<FileSnapshot>,
    sender: SyncSender<Command>,
    receiver: Mutex<Option<Receiver<Command>>>,
    quiescing: AtomicBool,
    quiesced: AtomicBool,
    pending_import: Mutex<Option<PendingImport>>,
}
#[derive(Clone)]
pub struct PhoneFiles {
    inner: Arc<Inner>,
}
impl Default for PhoneFiles {
    fn default() -> Self {
        let (sender, receiver) = mpsc::sync_channel(1);
        Self {
            inner: Arc::new(Inner {
                state: Mutex::new(FileSnapshot::default()),
                sender,
                receiver: Mutex::new(Some(receiver)),
                quiescing: AtomicBool::new(false),
                quiesced: AtomicBool::new(false),
                pending_import: Mutex::new(None),
            }),
        }
    }
}
impl PhoneFiles {
    pub fn snapshot(&self) -> FileSnapshot {
        self.inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
    pub fn observe_serial(&self, cable_present: bool, input_ready: bool) {
        self.update(|state| state.serial_ready = cable_present && (state.serial_ready || input_ready));
    }
    pub fn pending_commit(&self) -> bool {
        self.inner
            .pending_import
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some()
    }
    fn defer_source_delete(&self, source: &Path, expected: (u64, [u8; 32])) -> Result<()> {
        let id = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .job
            .as_ref()
            .context("File job is missing")?
            .id;
        let mut pending = self
            .inner
            .pending_import
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        ensure!(
            pending.is_none(),
            "Another imported file is waiting for a save"
        );
        *pending = Some(PendingImport {
            id,
            source: source.to_owned(),
            expected,
        });
        Ok(())
    }
    /// Called only after a healthy VM checkpoint and its metadata are durable.
    pub fn commit_saved(&self) -> Result<()> {
        let Some(pending) = self
            .inner
            .pending_import
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
        else {
            return Ok(());
        };
        let result = (|| {
            let mut source = fs::File::open(&pending.source)?;
            ensure!(
                digest(&mut source)? == pending.expected,
                "Imported file was saved, but the original changed; the original was kept"
            );
            drop(source);
            fs::remove_file(&pending.source)
                .context("Imported file was saved, but the original could not be removed")?;
            Ok(())
        })();
        self.update(|state| {
            state.busy = false;
            if let Some(job) = &mut state.job {
                if job.id == pending.id {
                    match &result {
                        Ok(()) => {
                            job.phase = "complete".into();
                            job.error = None;
                        }
                        Err(error) => {
                            job.phase = "error".into();
                            job.error = Some(format!("{error:#}"));
                        }
                    }
                }
            }
        });
        result
    }
    pub fn fail_pending_commit(&self, message: &str) {
        let pending = self
            .inner
            .pending_import
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        if let Some(pending) = pending {
            self.update(|state| {
                state.busy = false;
                if let Some(job) = &mut state.job {
                    if job.id == pending.id {
                        job.phase = "error".into();
                        job.error = Some(
                            format!("Imported file was not saved; the source was kept. {message}")
                                .chars()
                                .take(4096)
                                .collect(),
                        );
                    }
                }
            });
        }
    }
    /// The owner resumes the VM after a live checkpoint. The disconnected
    /// client can then perform a fresh ASCII handover for the next operation.
    pub fn resume_after_save(&self) {
        self.inner.quiesced.store(false, Ordering::Release);
        self.inner.quiescing.store(false, Ordering::Release);
    }
    pub fn request(&self, request: FileRequest) -> Result<u64> {
        request.validate()?;
        let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        ensure!(
            !self.inner.quiescing.load(Ordering::Acquire),
            "The phone is stopping; retry after it resumes"
        );
        ensure!(state.attached, "Start the phone before opening its files");
        ensure!(!state.busy, "Another phone file operation is in progress");
        let id = NONCE.fetch_add(1, Ordering::Relaxed);
        state.busy = true;
        state.job = Some(FileJob {
            id,
            operation: request.operation().into(),
            phase: "queued".into(),
            bytes: 0,
            total: None,
            error: None,
            host: None,
        });
        if let Err(error) = self.inner.sender.try_send(Command::Request(id, request)) {
            state.busy = false;
            if let Some(job) = &mut state.job {
                job.phase = "error".into();
                job.error = Some(format!("File worker is unavailable: {error}"));
            }
            bail!("File worker is unavailable: {error}");
        }
        Ok(id)
    }
    /// Serialize a disconnect behind every accepted operation while the guest
    /// is still running. A failed barrier must prevent a new checkpoint.
    pub fn quiesce(&self, timeout: Duration) -> Result<()> {
        let deadline = Instant::now() + timeout;
        {
            let state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            if self.inner.quiescing.swap(true, Ordering::AcqRel) {
                ensure!(
                    self.inner.quiesced.load(Ordering::Acquire),
                    "File service is already stopping"
                );
                return Ok(());
            }
            if !state.attached && !state.busy {
                self.inner.quiesced.store(true, Ordering::Release);
                return Ok(());
            }
        }
        let (sender, receiver) = mpsc::sync_channel(1);
        let mut command = Command::Quiesce(sender);
        loop {
            ensure!(Instant::now() < deadline, "File operation did not finish before the checkpoint deadline; the previous saved state was kept");
            match self.inner.sender.try_send(command) {
                Ok(()) => break,
                Err(mpsc::TrySendError::Full(pending)) => {
                    command = pending;
                    thread::sleep(Duration::from_millis(10));
                }
                Err(mpsc::TrySendError::Disconnected(_)) => {
                    bail!("File worker disconnected before checkpoint")
                }
            }
        }
        match receiver.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(Ok(())) => {
                self.inner.quiesced.store(true, Ordering::Release);
                Ok(())
            }
            Ok(Err(error)) => bail!("File service could not disconnect safely: {error}"),
            Err(mpsc::RecvTimeoutError::Timeout) => bail!(
                "File service checkpoint deadline exceeded; the previous saved state was kept"
            ),
            Err(_) => bail!("File worker disconnected before checkpoint"),
        }
    }
    fn update(&self, update: impl FnOnce(&mut FileSnapshot)) {
        update(&mut self.inner.state.lock().unwrap_or_else(|e| e.into_inner()));
    }
    fn progress(&self, bytes: u64, total: Option<u64>) {
        self.update(|state| {
            if let Some(job) = &mut state.job {
                job.phase = "transferring".into();
                job.bytes = bytes;
                job.total = total;
            }
        });
    }
}

/// A localhost socket connects the host client to firmware UART0. This is a
/// PC Suite style cable connection; Bluetooth has a separate host bridge.
pub struct SerialTransport {
    port: u16,
    stop: Arc<AtomicBool>,
    socket: Arc<Mutex<Option<TcpStream>>>,
    handle: Option<JoinHandle<()>>,
    files: PhoneFiles,
}
impl SerialTransport {
    pub fn bind(files: PhoneFiles) -> Result<Self> {
        Self::bind_in(files, &std::env::temp_dir())
    }
    pub fn bind_in(files: PhoneFiles, temporary: &Path) -> Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();
        let receiver = files
            .inner
            .receiver
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
            .context("Phone file handle is already attached")?;
        let temporary = temporary.to_owned();
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = stop.clone();
        let socket = Arc::new(Mutex::new(None));
        let thread_socket = socket.clone();
        let thread_files = files.clone();
        let handle = thread::Builder::new()
            .name("phone-files".into())
            .spawn(move || {
                let stream = loop {
                    if thread_stop.load(Ordering::Relaxed) {
                        return;
                    }
                    match listener.accept() {
                        Ok((stream, address)) if address.ip().is_loopback() => break stream,
                        Ok(_) => {}
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(10))
                        }
                        Err(e) => {
                            thread_files.update(|s| {
                                s.job = Some(FileJob {
                                    id: 0,
                                    operation: "attach".into(),
                                    phase: "error".into(),
                                    bytes: 0,
                                    total: None,
                                    error: Some(e.to_string()),
                                    host: None,
                                })
                            });
                            return;
                        }
                    }
                };
                let _ = stream.set_nodelay(true);
                let _ = stream.set_read_timeout(Some(Duration::from_millis(250)));
                let _ = stream.set_write_timeout(Some(Duration::from_millis(250)));
                *thread_socket.lock().unwrap_or_else(|e| e.into_inner()) = stream.try_clone().ok();
                let mut client = ObexClient::new(stream);
                thread_files.update(|s| s.attached = true);
                while !thread_stop.load(Ordering::Relaxed) {
                    let (id, request) = match receiver.recv_timeout(Duration::from_millis(100)) {
                        Ok(Command::Request(id, request)) => (id, request),
                        Ok(Command::Quiesce(reply)) => {
                            let result = (|| {
                                ensure!(
                                    !client.failed(),
                                    "The transfer stream failed; the previous saved state was kept"
                                );
                                client.disconnect()?;
                                Ok(())
                            })()
                            .map_err(|error: anyhow::Error| format!("{error:#}"));
                            thread_files.update(|s| s.connected = client.is_connected());
                            let _ = reply.send(result);
                            continue;
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => continue,
                        Err(_) => break,
                    };
                    thread_files.update(|s| {
                        if let Some(job) = &mut s.job {
                            if job.id == id {
                                job.phase = "connecting".into();
                            }
                        }
                    });
                    let result = (|| {
                        match &request {
                            FileRequest::ImportInternal { .. } => client.connect_when_ready()?,
                            FileRequest::Disconnect => {}
                            _ => client.connect(false)?,
                        }
                        thread_files.update(|s| s.connected = client.is_connected());
                        perform(&mut client, &request, &thread_files, &temporary)
                    })();
                    let awaiting_save = result.is_ok() && thread_files.pending_commit();
                    thread_files.update(|s| {
                        s.busy = awaiting_save;
                        s.connected = client.is_connected();
                        if let Some(job) = &mut s.job {
                            if job.id == id {
                                match result {
                                    Ok(host) => {
                                        job.phase = if awaiting_save {
                                            "awaiting_save"
                                        } else {
                                            "complete"
                                        }
                                        .into();
                                        job.host = host;
                                    }
                                    Err(error) => {
                                        job.phase = "error".into();
                                        job.error =
                                            Some(format!("{error:#}").chars().take(4096).collect());
                                    }
                                }
                            }
                        }
                    });
                }
                thread_files.update(|s| {
                    s.attached = false;
                    s.connected = false;
                    s.busy = false;
                });
            })?;
        Ok(Self {
            port,
            stop,
            socket,
            handle: Some(handle),
            files,
        })
    }
    pub fn chardev(&self) -> String {
        format!(
            "socket,id=uart0,host=127.0.0.1,port={},reconnect-ms=0,nodelay=on",
            self.port
        )
    }
}
impl Drop for SerialTransport {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(socket) = self.socket.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = socket.shutdown(Shutdown::Both);
        }
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
        self.files
            .fail_pending_commit("The phone stopped before the save finished.");
        self.files.update(|s| {
            s.attached = false;
            s.connected = false;
            s.busy = false;
        });
    }
}

struct Temporary {
    path: PathBuf,
    file: Option<fs::File>,
}
impl Temporary {
    fn new(directory: &Path) -> Result<Self> {
        fs::create_dir_all(directory)?;
        for _ in 0..64 {
            let id = NONCE.fetch_add(1, Ordering::Relaxed);
            let path = directory.join(format!(".ph0nix-transfer-{}-{id}.part", std::process::id()));
            match fs::OpenOptions::new()
                .create_new(true)
                .read(true)
                .write(true)
                .open(&path)
            {
                Ok(file) => {
                    return Ok(Self {
                        path,
                        file: Some(file),
                    })
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(e.into()),
            }
        }
        bail!("Unable to reserve transfer staging file")
    }
    fn file(&mut self) -> &mut fs::File {
        self.file.as_mut().expect("live staging file")
    }
}
impl Drop for Temporary {
    fn drop(&mut self) {
        self.file = None;
        let _ = fs::remove_file(&self.path);
    }
}

pub(crate) fn digest(file: &mut impl Read) -> Result<(u64, [u8; 32])> {
    let mut sha = Sha256::new();
    let mut count = 0;
    let mut buffer = [0; 32768];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        count += n as u64;
        ensure!(
            count <= MAX_FILE_BYTES as u64,
            "File exceeds 32 MiB transfer limit"
        );
        sha.update(&buffer[..n]);
    }
    Ok((count, sha.finalize().into()))
}
pub(crate) fn expected_digest(bytes: u64, sha256: &str) -> Result<(u64, [u8; 32])> {
    ensure!(
        bytes <= MAX_FILE_BYTES as u64 && sha256.len() == 64,
        "Invalid expected file identity"
    );
    let mut hash = [0; 32];
    for (index, pair) in sha256.as_bytes().chunks_exact(2).enumerate() {
        let high = (pair[0] as char)
            .to_digit(16)
            .context("Invalid expected SHA-256")?;
        let low = (pair[1] as char)
            .to_digit(16)
            .context("Invalid expected SHA-256")?;
        hash[index] = ((high << 4) | low) as u8;
    }
    Ok((bytes, hash))
}
fn verify_remote<T: Read + Write>(
    client: &mut ObexClient<T>,
    path: &[String],
    expected: (u64, [u8; 32]),
) -> Result<()> {
    struct DigestSink {
        sha: Sha256,
        count: u64,
    }
    impl Write for DigestSink {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.sha.update(b);
            self.count += b.len() as u64;
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut sink = DigestSink {
        sha: Sha256::new(),
        count: 0,
    };
    client.get(path, &mut sink, |_, _| {})?;
    ensure!(
        sink.count == expected.0 && <[u8; 32]>::from(sink.sha.finalize()) == expected.1,
        "Destination verification failed; source file was kept"
    );
    Ok(())
}
fn existing<T: Read + Write>(client: &mut ObexClient<T>, path: &[String]) -> Result<bool> {
    let (name, parent) = path.split_last().context("File name required")?;
    Ok(client
        .list(parent)?
        .iter()
        .any(|entry| entry.name.eq_ignore_ascii_case(name)))
}
/// Locate K750i's internal Other directory using the firmware's own listing.
pub fn internal_other_path(
    mut list: impl FnMut(&[String]) -> Result<Vec<FileEntry>>,
) -> Result<Vec<String>> {
    fn folder<'a>(entries: &'a [FileEntry], names: &[&str]) -> Option<&'a str> {
        entries
            .iter()
            .find(|entry| {
                entry.directory
                    && names.iter().any(|name| entry.name.to_lowercase() == name.to_lowercase())
            })
            .map(|entry| entry.name.as_str())
    }

    let root = list(&[])?;
    let names = ["Other", "Прочее", "Другое"];
    if let Some(memory) = folder(&root, &["Phone memory", "Память телефона"]) {
        let mut path = vec![memory.to_owned()];
        let entries = list(&path)?;
        let other = folder(&entries, &names).context("No Other directory in phone memory")?;
        path.push(other.to_owned());
        Ok(path)
    } else {
        // Without a separate phone memory container, user folders appear at the root.
        let other = folder(&root, &names).context("No internal Other directory in OBEX root")?;
        Ok(vec![other.to_owned()])
    }
}

fn import_file<T: Read + Write>(
    client: &mut ObexClient<T>,
    files: &PhoneFiles,
    path: &[String],
    host: &Path,
    replace: bool,
    move_source: bool,
) -> Result<()> {
    use std::io::{Seek, SeekFrom};
    let mut source = fs::File::open(host)?;
    ensure!(source.metadata()?.is_file(), "Choose a regular file");
    let identity = digest(&mut source)?;
    if !replace {
        ensure!(
            !existing(client, path)?,
            "Phone file already exists; choose another name or enable replace"
        );
    }
    source.seek(SeekFrom::Start(0))?;
    client.put(path, &mut source, identity.0, |n, t| files.progress(n, t))?;
    verify_remote(client, path, identity)?;
    if move_source {
        files.defer_source_delete(host, identity)?;
    }
    Ok(())
}

fn perform<T: Read + Write>(
    client: &mut ObexClient<T>,
    request: &FileRequest,
    files: &PhoneFiles,
    temporary: &Path,
) -> Result<Option<PathBuf>> {
    use std::io::{Seek, SeekFrom};
    match request {
        FileRequest::List { path } => {
            let entries = client.list(path)?;
            files.update(|s| {
                s.path = path.clone();
                s.entries = entries;
            });
        }
        FileRequest::Import {
            path,
            host,
            replace,
            move_source,
        } => {
            import_file(client, files, path, host, *replace, *move_source)?;
        }
        FileRequest::ImportInternal { name, host } => {
            let mut path = internal_other_path(|path| client.list(path))?;
            path.push(name.clone());
            import_file(client, files, &path, host, false, true)?;
        }
        FileRequest::Export {
            path,
            host,
            move_source,
        } => {
            ensure!(!host.exists(), "Destination file already exists");
            let parent = host.parent().context("Destination directory is missing")?;
            let mut staged = Temporary::new(parent)?;
            client.get(path, staged.file(), |n, t| files.progress(n, t))?;
            staged.file().sync_all()?;
            staged.file().seek(SeekFrom::Start(0))?;
            let identity = digest(staged.file())?;
            // Reserve a destination without overwriting, then stream into it.
            let mut destination = fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(host)?;
            staged.file().seek(SeekFrom::Start(0))?;
            let write = (|| {
                std::io::copy(staged.file(), &mut destination)?;
                destination.sync_all()?;
                let mut verify = fs::File::open(host)?;
                ensure!(
                    digest(&mut verify)? == identity,
                    "Export verification failed"
                );
                Ok(())
            })();
            if let Err(error) = write {
                drop(destination);
                let _ = fs::remove_file(host);
                return Err(error);
            }
            if *move_source {
                verify_remote(client, path, identity)?;
                client.delete(path)?;
            }
            return Ok(Some(host.clone()));
        }
        FileRequest::Copy {
            source,
            destination,
            replace,
            move_source,
        } => {
            if !replace {
                ensure!(
                    !existing(client, destination)?,
                    "Destination phone file already exists"
                );
            }
            let mut staged = Temporary::new(temporary)?;
            client.get(source, staged.file(), |n, t| files.progress(n, t))?;
            staged.file().seek(SeekFrom::Start(0))?;
            let identity = digest(staged.file())?;
            staged.file().seek(SeekFrom::Start(0))?;
            client.put(destination, staged.file(), identity.0, |n, t| {
                files.progress(n, t)
            })?;
            verify_remote(client, destination, identity)?;
            if *move_source {
                verify_remote(client, source, identity)?;
                client.delete(source)?;
            }
        }
        FileRequest::MakeDirectory { path } => client.mkdir(path)?,
        FileRequest::Delete { path } => client.delete(path)?,
        FileRequest::DeleteVerified {
            path,
            bytes,
            sha256,
        } => {
            let expected = expected_digest(*bytes, sha256)?;
            verify_remote(client, path, expected)
                .context("Source changed since export; the source file was kept")?;
            client.delete(path)?;
        }
        FileRequest::Disconnect => client.disconnect()?,
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};

    fn test_packet(status: u8, body: &[u8]) -> Vec<u8> {
        let mut packet = vec![status];
        packet.extend_from_slice(&((body.len() + 3) as u16).to_be_bytes());
        packet.extend_from_slice(body);
        packet
    }
    fn read_packet(stream: &mut impl Read) -> Vec<u8> {
        let mut prefix = [0; 3];
        stream.read_exact(&mut prefix).unwrap();
        let length = u16::from_be_bytes([prefix[1], prefix[2]]) as usize;
        assert!((3..=4096).contains(&length));
        let mut packet = prefix.to_vec();
        packet.resize(length, 0);
        stream.read_exact(&mut packet[3..]).unwrap();
        packet
    }
    #[test]
    fn detached_and_invalid_requests_do_not_create_jobs() {
        let files = PhoneFiles::default();
        assert!(files.request(FileRequest::List { path: vec![] }).is_err());
        assert!(files.request(FileRequest::Delete { path: vec![] }).is_err());
        assert!(!files.snapshot().busy);
    }
    #[test]
    fn json_request_uses_stable_operation_schema() {
        let request: FileRequest = serde_json::from_str(
            r#"{"operation":"import","path":["Pictures","a.png"],"host":"/data/a.png"}"#,
        )
        .unwrap();
        assert!(matches!(
            request,
            FileRequest::Import {
                replace: false,
                move_source: false,
                ..
            }
        ));
        assert!(serde_json::from_str::<FileRequest>(
            r#"{"operation":"delete","path":[],"typo":true}"#
        )
        .is_err());
    }

    fn directories(names: &[&str]) -> Vec<FileEntry> {
        names.iter().map(|name| FileEntry {
            name: (*name).into(),
            directory: true,
            size: None,
            modified: None,
        }).collect()
    }

    #[test]
    fn internal_other_is_found_at_the_root_without_a_memory_container() {
        let path = internal_other_path(|path| {
            assert!(path.is_empty());
            Ok(directories(&["Pictures", "Other"]))
        }).unwrap();
        assert_eq!(path, ["Other"]);
    }

    #[test]
    fn internal_other_uses_the_localized_phone_container_and_ignores_card_folders() {
        let path = internal_other_path(|path| match path {
            [] => Ok(directories(&["Memory Stick", "ПАМЯТЬ ТЕЛЕФОНА", "Other"])),
            [memory] if memory == "ПАМЯТЬ ТЕЛЕФОНА" => Ok(directories(&["Прочее"])),
            _ => panic!("Unexpected listing of {}", path.join("/")),
        }).unwrap();
        assert_eq!(path, ["ПАМЯТЬ ТЕЛЕФОНА", "Прочее"]);
    }

    #[test]
    fn internal_other_never_falls_back_to_the_card() {
        let error = internal_other_path(|path| match path {
            [] => Ok(directories(&["Memory Stick", "Phone memory", "Other"])),
            [memory] if memory == "Phone memory" => Ok(directories(&["Pictures"])),
            _ => panic!("Unexpected listing of {}", path.join("/")),
        }).unwrap_err();
        assert_eq!(error.to_string(), "No Other directory in phone memory");
    }

    #[test]
    fn case_alias_moves_cannot_delete_the_destination() {
        let request = FileRequest::Copy {
            source: vec!["Phone memory".into(), "Other".into(), "a.txt".into()],
            destination: vec!["phone memory".into(), "other".into(), "A.TXT".into()],
            replace: true,
            move_source: true,
        };
        assert!(request.validate().is_err());
    }

    #[test]
    fn verified_delete_rejects_invalid_identity_and_preserves_changed_phone_file() {
        let request = FileRequest::DeleteVerified {
            path: vec!["x.txt".into()],
            bytes: 3,
            sha256: Sha256::digest(b"old")
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect(),
        };
        assert!(request.validate().is_ok());
        assert!(FileRequest::DeleteVerified {
            path: vec!["x.txt".into()],
            bytes: 3,
            sha256: "bad".into()
        }
        .validate()
        .is_err());
        struct Script {
            read: std::io::Cursor<Vec<u8>>,
            write: Vec<u8>,
        }
        impl Read for Script {
            fn read(&mut self, b: &mut [u8]) -> std::io::Result<usize> {
                self.read.read(b)
            }
        }
        impl Write for Script {
            fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
                self.write.extend_from_slice(b);
                Ok(b.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let mut connect = vec![0x10, 0, 0x10, 0, 0x4a, 0, 0x13];
        connect.extend_from_slice(&[
            0xf9, 0xec, 0x7b, 0xc4, 0x95, 0x3c, 0x11, 0xd2, 0x98, 0x4e, 0x52, 0x54, 0, 0xdc, 0x9e,
            9,
        ]);
        connect.extend_from_slice(&[0xcb, 0, 0, 0, 1]);
        let mut responses = test_packet(0xa0, &connect);
        responses.extend(test_packet(0xa0, &[]));
        responses.extend(test_packet(0xa0, &[0x49, 0, 6, b'n', b'e', b'w']));
        let mut client = ObexClient::new(Script {
            read: std::io::Cursor::new(responses),
            write: Vec::new(),
        });
        client.connect(true).unwrap();
        let result = perform(
            &mut client,
            &request,
            &PhoneFiles::default(),
            &std::env::temp_dir(),
        );
        assert!(result.unwrap_err().to_string().contains("Source changed"));
        // A delete would need a further response; success means the changed
        // contents were rejected before any DELETE packet was attempted.
        assert!(!client.failed());
    }

    #[test]
    fn quiesce_finishes_accepted_job_then_disconnects_without_replacing_its_result() {
        let files = PhoneFiles::default();
        let transport = SerialTransport::bind(files.clone()).unwrap();
        let (seen_sender, seen_receiver) = mpsc::sync_channel(1);
        let (release_sender, release_receiver) = mpsc::sync_channel(1);
        let port = transport.port;
        let peer = thread::spawn(move || {
            let stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut stream = BufReader::new(stream);
            let mut line = Vec::new();
            stream.read_until(b'\r', &mut line).unwrap();
            assert_eq!(line, b"AT+CPROT=0\r");
            stream.get_mut().write_all(b"\r\nCONNECT\r\n").unwrap();
            assert_eq!(read_packet(&mut stream)[0], 0x80);
            let mut response = vec![0x10, 0, 0x10, 0, 0x4a, 0, 0x13];
            response.extend_from_slice(&[
                0xf9, 0xec, 0x7b, 0xc4, 0x95, 0x3c, 0x11, 0xd2, 0x98, 0x4e, 0x52, 0x54, 0, 0xdc,
                0x9e, 9,
            ]);
            response.extend_from_slice(&[0xcb, 0, 0, 0, 1]);
            stream
                .get_mut()
                .write_all(&test_packet(0xa0, &response))
                .unwrap();
            assert_eq!(read_packet(&mut stream)[0], 0x85);
            stream.get_mut().write_all(&test_packet(0xa0, &[])).unwrap();
            assert_eq!(read_packet(&mut stream)[0], 0x83);
            seen_sender.send(()).unwrap();
            release_receiver
                .recv_timeout(Duration::from_secs(2))
                .unwrap();
            let xml = b"<folder-listing version=\"1.0\"/>";
            let mut body = vec![0x49];
            body.extend_from_slice(&((xml.len() + 3) as u16).to_be_bytes());
            body.extend_from_slice(xml);
            stream
                .get_mut()
                .write_all(&test_packet(0xa0, &body))
                .unwrap();
            assert_eq!(read_packet(&mut stream)[0], 0x81);
            stream.get_mut().write_all(&test_packet(0xa0, &[])).unwrap();
        });
        let deadline = Instant::now() + Duration::from_secs(2);
        while !files.snapshot().attached {
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(1));
        }
        let id = files.request(FileRequest::List { path: vec![] }).unwrap();
        seen_receiver.recv_timeout(Duration::from_secs(2)).unwrap();
        let stopper = files.clone();
        let done = thread::spawn(move || stopper.quiesce(Duration::from_secs(2)));
        while !files.inner.quiescing.load(Ordering::Acquire) {
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(1));
        }
        assert!(files.request(FileRequest::List { path: vec![] }).is_err());
        release_sender.send(()).unwrap();
        done.join().unwrap().unwrap();
        peer.join().unwrap();
        let snapshot = files.snapshot();
        assert!(!snapshot.connected);
        assert!(!snapshot.busy);
        assert_eq!(snapshot.job.as_ref().unwrap().id, id);
        assert_eq!(snapshot.job.as_ref().unwrap().phase, "complete");
        files.quiesce(Duration::from_millis(1)).unwrap();
        drop(transport);
    }

    #[test]
    fn missing_disconnect_ack_is_bounded_and_never_allows_new_requests() {
        let files = PhoneFiles::default();
        files.update(|state| state.attached = true);
        let start = Instant::now();
        assert!(files.quiesce(Duration::from_millis(20)).is_err());
        assert!(start.elapsed() < Duration::from_secs(1));
        assert!(files.request(FileRequest::List { path: vec![] }).is_err());
    }

    fn staged_import(source: &Path) -> PhoneFiles {
        let files = PhoneFiles::default();
        files.update(|state| {
            state.attached = true;
            state.busy = true;
            state.job = Some(FileJob {
                id: 42,
                operation: "import".into(),
                phase: "awaiting_save".into(),
                bytes: 3,
                total: Some(3),
                error: None,
                host: None,
            });
        });
        let identity = digest(&mut fs::File::open(source).unwrap()).unwrap();
        files.defer_source_delete(source, identity).unwrap();
        files
    }
    struct ImportDirectory(PathBuf);
    impl ImportDirectory {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "ph0nix-import-commit-{}-{}",
                std::process::id(),
                NONCE.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for ImportDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn imported_move_retains_host_until_durable_ack_and_quiesces_without_deadlock() {
        let directory = ImportDirectory::new();
        let source = directory.0.join("source");
        fs::write(&source, b"old").unwrap();
        let files = staged_import(&source);
        assert!(source.exists());
        assert!(files.pending_commit());
        assert!(files.request(FileRequest::List { path: vec![] }).is_err());
        let receiver = files.inner.receiver.lock().unwrap().take().unwrap();
        let server =
            thread::spawn(
                move || match receiver.recv_timeout(Duration::from_secs(1)).unwrap() {
                    Command::Quiesce(reply) => reply.send(Ok(())).unwrap(),
                    _ => panic!("Expected the disconnect barrier, not another user job"),
                },
            );
        files.quiesce(Duration::from_secs(1)).unwrap();
        server.join().unwrap();
        assert!(source.exists());
        assert_eq!(files.snapshot().job.unwrap().phase, "awaiting_save");
        files.commit_saved().unwrap();
        assert!(!source.exists());
        assert!(!files.pending_commit());
        let snapshot = files.snapshot();
        assert!(!snapshot.busy);
        assert_eq!(snapshot.job.as_ref().unwrap().id, 42);
        assert_eq!(snapshot.job.unwrap().phase, "complete");
    }
    #[test]
    fn checkpoint_failure_preserves_imported_host_source_and_reports_original_job() {
        let directory = ImportDirectory::new();
        let source = directory.0.join("source");
        fs::write(&source, b"old").unwrap();
        let files = staged_import(&source);
        files.fail_pending_commit("Disk is full");
        assert_eq!(fs::read(&source).unwrap(), b"old");
        assert!(!files.pending_commit());
        let snapshot = files.snapshot();
        assert!(!snapshot.busy);
        let job = snapshot.job.unwrap();
        assert_eq!(job.id, 42);
        assert_eq!(job.phase, "error");
        assert!(job.error.unwrap().contains("source was kept"));
    }
    #[test]
    fn durable_import_ack_does_not_delete_same_size_changed_host_source() {
        let directory = ImportDirectory::new();
        let source = directory.0.join("source");
        fs::write(&source, b"old").unwrap();
        let files = staged_import(&source);
        fs::write(&source, b"new").unwrap();
        assert!(files.commit_saved().is_err());
        assert_eq!(fs::read(&source).unwrap(), b"new");
        assert_eq!(files.snapshot().job.unwrap().phase, "error");
        assert!(!files.pending_commit());
    }
}
