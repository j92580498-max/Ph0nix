// SPDX-License-Identifier: GPL-2.0-or-later
//! A bounded, disposable PCM transport. Speakers never pace the guest CPU.
use anyhow::{ensure, Context, Result};
use serde::Deserialize;
use std::{collections::VecDeque, io::{BufReader, Read}, net::{TcpListener, TcpStream},
    sync::{atomic::{AtomicBool, Ordering}, Arc, Mutex}, thread::{self, JoinHandle},
    time::{Duration, Instant}};
use crate::diagnostics::DiagnosticsHandle;

pub const SAMPLE_RATE: u32 = 48_000;
pub const MAX_FRAMES: usize = 960;
const MAX_PACKETS: usize = 10;

#[derive(Clone, Debug)]
pub struct PlaybackPacket {
    pub pcm: Vec<u8>,
    pub discontinuity: bool,
    pub eos: bool,
}

impl PlaybackPacket {
    /// JNI envelope: little-endian flags (bit 0 flush, bit 1 EOS), then s16le stereo.
    pub fn wire(self) -> Vec<u8> {
        let flags = u32::from(self.discontinuity) | (u32::from(self.eos) << 1);
        let mut bytes = Vec::with_capacity(4 + self.pcm.len());
        bytes.extend_from_slice(&flags.to_le_bytes());
        bytes.extend_from_slice(&self.pcm);
        bytes
    }
}

#[derive(Debug, Default, Clone)]
pub struct AudioStats {
    pub packets: u64,
    pub frames: u64,
    pub nonzero_samples: u64,
    pub dropped_frames: u64,
    pub invalid_connections: u64,
}

#[derive(Default)]
struct Queue { packets: VecDeque<PlaybackPacket>, stats: AudioStats, diagnostics: Option<DiagnosticsHandle> }

#[derive(Clone, Default)]
pub struct AudioBuffer { inner: Arc<Mutex<Queue>> }
impl AudioBuffer {
    pub fn take(&self) -> Option<PlaybackPacket> {
        let mut queue = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let packet = queue.packets.pop_front();
        Self::observe(&queue);
        packet
    }
    pub fn stats(&self) -> AudioStats {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).stats.clone()
    }
    pub fn set_diagnostics(&self, diagnostics: DiagnosticsHandle) {
        let mut queue = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        queue.diagnostics = Some(diagnostics);
        Self::observe(&queue);
    }
    fn observe(queue: &Queue) {
        if let Some(diagnostics) = &queue.diagnostics {
            let bytes = queue.packets.iter().map(|p| p.pcm.len()).sum::<usize>();
            diagnostics.record_audio(&queue.stats, (bytes / 4) as u64, bytes);
        }
    }
    fn clear(queue: &mut Queue) {
        queue.stats.dropped_frames += queue.packets.iter().map(|p| (p.pcm.len() / 4) as u64).sum::<u64>();
        queue.packets.clear();
    }
    fn push(&self, mut packet: PlaybackPacket) {
        let mut queue = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if packet.discontinuity || queue.packets.len() >= MAX_PACKETS {
            Self::clear(&mut queue);
            packet.discontinuity = true;
        }
        queue.stats.packets += 1;
        queue.stats.frames += (packet.pcm.len() / 4) as u64;
        queue.stats.nonzero_samples += packet.pcm.chunks_exact(2).filter(|p| p[0] != 0 || p[1] != 0).count() as u64;
        queue.packets.push_back(packet);
        Self::observe(&queue);
    }
    fn reset(&self) {
        self.push(PlaybackPacket { pcm: Vec::new(), discontinuity: true, eos: false });
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Header {
    #[serde(rename = "type")] kind: String,
    version: u32, stream_id: String,
    epoch: String, sequence: String, first_frame: String, virtual_start_ns: String,
    sample_rate: u32, channels: u32, format: String, frame_count: usize,
    discontinuity: bool, eos: bool,
}

fn counter(text: &str) -> Result<u64> {
    ensure!(!text.is_empty() && text.len() <= 20 && text.bytes().all(|c| c.is_ascii_digit()), "invalid audio counter");
    let value: u64 = text.parse()?;
    ensure!(value.to_string() == text, "noncanonical audio counter");
    Ok(value)
}

struct Packet { header: Header, epoch: u64, sequence: u64, first: u64, pcm: Vec<u8>, parse_time: Duration }
fn read_packet(reader: &mut impl Read) -> Result<Packet> {
    let mut prefix = [0; 4];
    reader.read_exact(&mut prefix)?;
    let size = u32::from_be_bytes(prefix) as usize;
    ensure!((1..=4096).contains(&size), "audio header exceeds its bound");
    let mut bytes = vec![0; size];
    reader.read_exact(&mut bytes)?;
    let parse_start = Instant::now();
    let header: Header = serde_json::from_slice(&bytes)?;
    ensure!(header.kind == "audio" && header.version == 1 && header.sample_rate == SAMPLE_RATE &&
        header.channels == 2 && header.format == "s16le" && header.frame_count <= MAX_FRAMES &&
        (header.frame_count != 0 || header.eos) && !header.stream_id.is_empty() &&
        header.stream_id.len() <= 64 && header.stream_id.is_ascii(), "unsupported PCM envelope");
    let epoch = counter(&header.epoch)?;
    let sequence = counter(&header.sequence)?;
    let first = counter(&header.first_frame)?;
    counter(&header.virtual_start_ns)?;
    first.checked_add(header.frame_count as u64).context("audio frame counter overflow")?;
    let mut pcm = vec![0; header.frame_count * 4];
    let parse_time = parse_start.elapsed();
    reader.read_exact(&mut pcm)?;
    Ok(Packet { header, epoch, sequence, first, pcm, parse_time })
}

#[derive(Default)]
struct Timeline { last: Option<(String, u64, u64, u64, bool)> }
impl Timeline {
    fn accept(&mut self, packet: Packet) -> Result<PlaybackPacket> {
        let h = packet.header;
        let mut discontinuity = h.discontinuity;
        if let Some((stream, epoch, sequence, end, ended)) = &self.last {
            if *stream == h.stream_id && *epoch == packet.epoch {
                ensure!(!ended && packet.sequence > *sequence && packet.first >= *end, "stale or overlapping PCM");
                if sequence.checked_add(1) != Some(packet.sequence) || packet.first != *end {
                    ensure!(discontinuity, "unannounced PCM gap");
                }
            } else {
                ensure!(*stream != h.stream_id || packet.epoch > *epoch, "stale PCM epoch");
                discontinuity = true;
            }
        } else { discontinuity = true; }
        self.last = Some((h.stream_id, packet.epoch, packet.sequence,
            packet.first + h.frame_count as u64, h.eos));
        Ok(PlaybackPacket { pcm: packet.pcm, discontinuity, eos: h.eos })
    }
}

/// Read timeouts retain partially received envelopes. Only shutdown interrupts them.
struct InterruptibleReader { socket: TcpStream, stop: Arc<AtomicBool> }
impl Read for InterruptibleReader {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        loop {
            if self.stop.load(Ordering::Relaxed) {
                // read_exact retries Interrupted forever; shutdown must end
                // the envelope and let the transport thread terminate.
                return Err(std::io::ErrorKind::UnexpectedEof.into());
            }
            match self.socket.read(out) {
                Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => continue,
                result => return result,
            }
        }
    }
}

pub struct AudioTransport { port: u16, stop: Arc<AtomicBool>, thread: Option<JoinHandle<()>> }
impl AudioTransport {
    pub fn bind(buffer: AudioBuffer) -> Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        let port = listener.local_addr()?.port();
        listener.set_nonblocking(true)?;
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let thread = thread::Builder::new().name("phone-pcm-transport".into()).spawn(move || {
            while !worker_stop.load(Ordering::Relaxed) {
                let socket = match listener.accept() {
                    Ok((socket, _)) => socket,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => { thread::sleep(Duration::from_millis(10)); continue; }
                    Err(_) => break,
                };
                if socket.set_read_timeout(Some(Duration::from_millis(100))).is_err() { continue; }
                // A packet has several short reads. Read-ahead avoids separate
                // TCP syscalls for the prefix, JSON envelope, and PCM payload.
                let mut reader = BufReader::with_capacity(16 * 1024, InterruptibleReader { socket, stop: worker_stop.clone() });
                let mut timeline = Timeline::default();
                loop {
                    let result = read_packet(&mut reader).and_then(|packet| {
                        let parse_time = packet.parse_time;
                        let started = Instant::now();
                        let accepted = timeline.accept(packet)?;
                        buffer.push(accepted);
                        if let Some(diagnostics) = &buffer.inner.lock().unwrap_or_else(|e| e.into_inner()).diagnostics {
                            diagnostics.record_audio_receive(parse_time + started.elapsed());
                        }
                        Ok(())
                    });
                    match result {
                        Ok(()) => {},
                        Err(error) => {
                            if !worker_stop.load(Ordering::Relaxed) && error.downcast_ref::<std::io::Error>()
                                .is_none_or(|e| e.kind() != std::io::ErrorKind::UnexpectedEof) {
                                let mut queue = buffer.inner.lock().unwrap_or_else(|e| e.into_inner());
                                queue.stats.invalid_connections += 1;
                                if let Some(diagnostics) = &queue.diagnostics {
                                    diagnostics.log("error", "audio", format!("PCM connection rejected: {error:#}"));
                                }
                                AudioBuffer::observe(&queue);
                            }
                            break;
                        }
                    }
                }
                buffer.reset();
            }
        })?;
        Ok(Self { port, stop, thread: Some(thread) })
    }
    pub fn chardev(&self) -> String {
        // Prefix, JSON envelope and PCM are separate engine writes. Do not
        // wait for delayed TCP acknowledgements before delivering a packet.
        format!("socket,id=audio,host=127.0.0.1,port={},server=off,reconnect-ms=100,nodelay=on", self.port)
    }
}
impl Drop for AudioTransport {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() { let _ = thread.join(); }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Write};
    fn wire(sequence: u64, first: u64, frames: usize, discontinuity: bool, eos: bool) -> Vec<u8> {
        let h = serde_json::to_vec(&serde_json::json!({"type":"audio","version":1,"stream_id":"test",
            "epoch":"7","sequence":sequence.to_string(),"first_frame":first.to_string(),
            "virtual_start_ns":"0","sample_rate":48000,"channels":2,"format":"s16le",
            "frame_count":frames,"discontinuity":discontinuity,"eos":eos})).unwrap();
        let mut out = (h.len() as u32).to_be_bytes().to_vec();
        out.extend(h); out.extend(std::iter::repeat_n(1, frames * 4)); out
    }
    #[test]
    fn pcm_timeline_rejects_replay_gaps_and_packets_after_eos() {
        let mut timeline = Timeline::default();
        let packet = |s, f, d, e| read_packet(&mut Cursor::new(wire(s, f, 2, d, e))).unwrap();
        assert!(timeline.accept(packet(0, 0, true, false)).unwrap().discontinuity);
        assert!(timeline.accept(packet(0, 0, false, false)).is_err());
        assert!(timeline.accept(packet(2, 4, false, false)).is_err());
        assert!(timeline.accept(packet(2, 4, true, false)).unwrap().discontinuity);
        assert!(timeline.accept(packet(3, 6, false, true)).unwrap().eos);
        assert!(timeline.accept(packet(4, 8, false, false)).is_err());
    }
    #[test]
    fn malformed_envelopes_are_bounded_before_allocating_pcm() {
        assert!(read_packet(&mut Cursor::new(4097u32.to_be_bytes())).is_err());
        assert!(read_packet(&mut Cursor::new(wire(0, 0, MAX_FRAMES + 1, true, false))).is_err());
        let mut truncated = wire(0, 0, 2, true, false); truncated.pop();
        assert!(read_packet(&mut Cursor::new(truncated)).is_err());
        for bad in ["", "01", "-1", "18446744073709551616"] { assert!(counter(bad).is_err()); }
        assert_eq!(counter("18446744073709551615").unwrap(), u64::MAX);
    }
    #[test]
    fn a_slow_speaker_cannot_grow_the_queue_or_replay_stale_audio() {
        let buffer = AudioBuffer::default();
        for _ in 0..11 { buffer.push(PlaybackPacket { pcm: vec![1; MAX_FRAMES * 4], discontinuity: false, eos: false }); }
        assert_eq!(buffer.stats().dropped_frames, 10 * MAX_FRAMES as u64);
        assert!(buffer.take().unwrap().discontinuity);
        assert!(buffer.take().is_none());
        buffer.push(PlaybackPacket { pcm: vec![1; 8], discontinuity: false, eos: true });
        buffer.reset();
        assert!(buffer.take().unwrap().pcm.is_empty());
        assert!(buffer.take().is_none());
    }
    #[test]
    fn fragmented_tcp_packets_reconnect_and_shutdown_without_a_speaker() {
        let buffer = AudioBuffer::default();
        let transport = AudioTransport::bind(buffer.clone()).unwrap();
        let mut peer = TcpStream::connect(("127.0.0.1", transport.port)).unwrap();
        let bytes = wire(0, 0, 2, true, false);
        for chunk in bytes.chunks(3) { peer.write_all(chunk).unwrap(); }
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while buffer.stats().frames == 0 { assert!(std::time::Instant::now() < deadline); thread::sleep(Duration::from_millis(5)); }
        assert_eq!(buffer.take().unwrap().pcm, [1; 8]);
        drop(peer);
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        loop { if buffer.take().is_some() { break; } assert!(std::time::Instant::now() < deadline); thread::sleep(Duration::from_millis(5)); }
        let mut peer = TcpStream::connect(("127.0.0.1", transport.port)).unwrap();
        peer.write_all(&[0, 0]).unwrap(); // Incomplete header, then shutdown.
        drop(transport);
    }
}
