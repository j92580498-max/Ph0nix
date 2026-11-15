// SPDX-License-Identifier: GPL-2.0-or-later
//! Completed RGB565 panels arrive independently of QMP and keypad requests.
use anyhow::{ensure, Result};
use std::{
    io::{BufReader, Read},
    net::{TcpListener, TcpStream},
    sync::{atomic::{AtomicBool, Ordering}, Arc, Mutex},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use crate::{diagnostics::DiagnosticsHandle, LcdFrame};

const PIXEL_BYTES: usize = 176 * 220 * 2;

fn read_frame(reader: &mut impl Read, diagnostics: &DiagnosticsHandle) -> Result<LcdFrame> {
    let mut header = [0; 24];
    reader.read_exact(&mut header)?;
    let started = Instant::now();
    ensure!(&header[..4] == b"K750"
        && u16::from_be_bytes(header[4..6].try_into().unwrap()) == 1
        && u16::from_be_bytes(header[6..8].try_into().unwrap()) == 1,
        "unsupported native LCD envelope");
    let width = u16::from_be_bytes(header[8..10].try_into().unwrap()) as usize;
    let height = u16::from_be_bytes(header[10..12].try_into().unwrap()) as usize;
    let length = u32::from_be_bytes(header[12..16].try_into().unwrap()) as usize;
    ensure!(width == 176 && height == 220 && length == PIXEL_BYTES,
        "native LCD dimensions or payload length differ");
    let revision = u64::from_be_bytes(header[16..24].try_into().unwrap());
    let mut rgb565_be = vec![0; PIXEL_BYTES];
    reader.read_exact(&mut rgb565_be)?;
    diagnostics.record_lcd_transport(started.elapsed());
    Ok(LcdFrame { width, height, rgb565_be, revision })
}

struct InterruptibleReader {
    socket: TcpStream,
    stop: Arc<AtomicBool>,
}

impl Read for InterruptibleReader {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        loop {
            if self.stop.load(Ordering::Relaxed) { return Ok(0); }
            match self.socket.read(bytes) {
                Err(error) if matches!(error.kind(), std::io::ErrorKind::WouldBlock
                    | std::io::ErrorKind::TimedOut | std::io::ErrorKind::Interrupted) => {},
                result => return result,
            }
        }
    }
}

#[derive(Default)]
struct Panel {
    latest: Option<LcdFrame>,
    error: Option<String>,
}

pub(crate) struct DisplayTransport {
    port: u16,
    panel: Arc<Mutex<Panel>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl DisplayTransport {
    pub fn bind(diagnostics: DiagnosticsHandle) -> Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();
        let panel = Arc::new(Mutex::new(Panel::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let worker_panel = panel.clone();
        let thread = thread::Builder::new().name("phone-lcd".into()).spawn(move || {
            while !worker_stop.load(Ordering::Relaxed) {
                let socket = match listener.accept() {
                    Ok((socket, _)) => socket,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                        continue;
                    },
                    Err(_) => break,
                };
                if socket.set_read_timeout(Some(Duration::from_millis(100))).is_err() { continue; }
                let mut reader = BufReader::with_capacity(128 * 1024,
                    InterruptibleReader { socket, stop: worker_stop.clone() });
                loop {
                    match read_frame(&mut reader, &diagnostics) {
                        Ok(frame) => {
                            // Drain the socket continuously, even if the GUI is
                            // stalled. It receives only the latest complete panel.
                            worker_panel.lock().unwrap_or_else(|e| e.into_inner()).latest = Some(frame);
                        },
                        Err(error) => {
                            if !worker_stop.load(Ordering::Relaxed)
                                && error.downcast_ref::<std::io::Error>().is_none_or(|e|
                                    matches!(e.kind(), std::io::ErrorKind::InvalidData)) {
                                let message = format!("Native LCD connection rejected: {error:#}");
                                diagnostics.log("error", "lcd", &message);
                                worker_panel.lock().unwrap_or_else(|e| e.into_inner()).error = Some(message);
                            }
                            break;
                        },
                    }
                }
            }
        })?;
        Ok(Self { port, panel, stop, thread: Some(thread) })
    }

    pub fn chardev(&self) -> String {
        format!("socket,id=lcd,host=127.0.0.1,port={},server=off,reconnect-ms=100,nodelay=on", self.port)
    }

    pub fn take(&self) -> Result<Option<LcdFrame>> {
        let mut panel = self.panel.lock().unwrap_or_else(|e| e.into_inner());
        ensure!(panel.error.is_none(), "{}", panel.error.as_deref().unwrap_or_default());
        Ok(panel.latest.take())
    }
}

impl Drop for DisplayTransport {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() { let _ = thread.join(); }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Write};

    fn wire(revision: u64) -> Vec<u8> {
        let mut bytes = b"K750".to_vec();
        for field in [1u16, 1, 176, 220] { bytes.extend_from_slice(&field.to_be_bytes()); }
        bytes.extend_from_slice(&(PIXEL_BYTES as u32).to_be_bytes());
        bytes.extend_from_slice(&revision.to_be_bytes());
        bytes.extend((0..PIXEL_BYTES).map(|i| i as u8));
        bytes
    }

    #[test]
    fn binary_panels_preserve_colors_and_validate_before_allocating_payloads() {
        let bytes = wire(42);
        let frame = read_frame(&mut Cursor::new(&bytes), &DiagnosticsHandle::default()).unwrap();
        assert_eq!((frame.width, frame.height, frame.revision), (176, 220, 42));
        assert_eq!(&frame.rgb565_be, &bytes[24..]);
        for offset in [0, 5, 7, 9, 11, 12, 15] {
            let mut invalid = bytes[..24].to_vec();
            invalid[offset] ^= 0xff;
            assert!(read_frame(&mut Cursor::new(invalid), &DiagnosticsHandle::default()).is_err());
        }
        assert!(read_frame(&mut Cursor::new(&bytes[..bytes.len()-1]), &DiagnosticsHandle::default()).is_err());
    }

    #[test]
    fn stalled_consumers_keep_the_latest_complete_panel_and_reconnect_cleanly() {
        let transport = DisplayTransport::bind(DiagnosticsHandle::default()).unwrap();
        for revisions in [[1, 2, 3], [4, 5, 6]] {
            let mut socket = TcpStream::connect(("127.0.0.1", transport.port)).unwrap();
            for revision in revisions { socket.write_all(&wire(revision)).unwrap(); }
            let deadline = Instant::now() + Duration::from_secs(2);
            loop {
                let revision = transport.panel.lock().unwrap().latest.as_ref().map(|f| f.revision);
                if revision == Some(revisions[2]) { break; }
                assert!(Instant::now() < deadline);
                thread::sleep(Duration::from_millis(1));
            }
            assert_eq!(transport.take().unwrap().unwrap().revision, revisions[2]);
            assert!(transport.take().unwrap().is_none());
        }
    }
}
