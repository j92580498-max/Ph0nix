// SPDX-License-Identifier: GPL-2.0-or-later
//! Bluetooth Object Push over a real host-adapter RFCOMM byte stream.
//!
//! This protocol is deliberately independent of the firmware's UART0 file
//! service and UART3 controller. Native apps stage received objects, then use
//! the phone's OBEX file service to import them. It does not emulate a radio.

pub mod platform;

use anyhow::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

pub const OBJECT_PUSH_UUID: &str = "00001105-0000-1000-8000-00805f9b34fb";
pub const MAX_OBJECT_BYTES: u64 = 32 * 1024 * 1024;
pub const RECEIVE_MTU: usize = 1024;
const SUCCESS: u8 = 0xa0;
const CONTINUE: u8 = 0x90;
const BAD_REQUEST: u8 = 0xc0;
const NOT_IMPLEMENTED: u8 = 0xd1;
static NEXT_STAGE: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReceivedFile {
    /// Original, validated file name. The private staging path is unique.
    pub name: String,
    pub path: PathBuf,
    pub bytes: u64,
    pub sha256: String,
}

/// Read one bounded OBEX packet from an RFCOMM stream. The caller configures
/// socket deadlines; no operation buffers a whole object in memory.
pub fn read_packet<R: Read>(stream: &mut R, maximum: usize) -> Result<Vec<u8>> {
    ensure!((3..=65535).contains(&maximum), "Invalid OBEX packet limit");
    let mut prefix = [0; 3];
    stream
        .read_exact(&mut prefix)
        .context("Bluetooth peer disconnected or timed out")?;
    let length = u16::from_be_bytes([prefix[1], prefix[2]]) as usize;
    ensure!(
        (3..=maximum).contains(&length),
        "Invalid Bluetooth OBEX packet length"
    );
    let mut packet = vec![0; length];
    packet[..3].copy_from_slice(&prefix);
    stream
        .read_exact(&mut packet[3..])
        .context("Incomplete Bluetooth OBEX packet")?;
    Ok(packet)
}

pub fn packet(opcode: u8, data: &[u8]) -> Result<Vec<u8>> {
    let length = data.len().checked_add(3).context("OBEX packet too large")?;
    ensure!(length <= 65535, "OBEX packet too large");
    let mut result = Vec::with_capacity(length);
    result.push(opcode);
    result.extend_from_slice(&(length as u16).to_be_bytes());
    result.extend_from_slice(data);
    Ok(result)
}

fn headers(mut bytes: &[u8]) -> Result<Vec<(u8, &[u8])>> {
    let mut result = Vec::new();
    while !bytes.is_empty() {
        let tag = bytes[0];
        let (length, offset) = match tag & 0xc0 {
            0 | 0x40 => {
                ensure!(bytes.len() >= 3, "Truncated Bluetooth OBEX header");
                let length = u16::from_be_bytes([bytes[1], bytes[2]]) as usize;
                ensure!(length >= 3, "Invalid Bluetooth OBEX header length");
                (length, 3)
            }
            0x80 => (2, 1),
            _ => (5, 1),
        };
        ensure!(length <= bytes.len(), "Truncated Bluetooth OBEX header");
        result.push((tag, &bytes[offset..length]));
        bytes = &bytes[length..];
    }
    Ok(result)
}

fn variable_header(tag: u8, value: &[u8]) -> Result<Vec<u8>> {
    ensure!(value.len() <= 65532, "OBEX header too large");
    let mut bytes = vec![tag];
    bytes.extend_from_slice(&((value.len() + 3) as u16).to_be_bytes());
    bytes.extend_from_slice(value);
    Ok(bytes)
}

fn connection_header(fields: &[(u8, &[u8])]) -> Result<Vec<u8>> {
    let ids: Vec<_> = fields.iter().filter(|(tag, _)| *tag == 0xcb).collect();
    ensure!(ids.len() <= 1, "Duplicate Bluetooth OBEX connection ID");
    Ok(if let Some((_, value)) = ids.first() {
        let mut result = vec![0xcb];
        result.extend_from_slice(value);
        result
    } else {
        vec![]
    })
}

fn validate_name(name: &str) -> Result<()> {
    ensure!(
        !name.is_empty() && !matches!(name, "." | "..") && name.encode_utf16().count() <= 255,
        "Invalid Bluetooth file name"
    );
    ensure!(
        !name
            .chars()
            .any(|c| c.is_control() || matches!(c, '/' | '\\' | ':')),
        "Bluetooth file name contains a path or control character"
    );
    Ok(())
}

fn decode_name(bytes: &[u8]) -> Result<String> {
    ensure!(
        bytes.len() >= 2 && bytes.len() % 2 == 0 && bytes.ends_with(&[0, 0]),
        "Invalid Bluetooth file-name encoding"
    );
    let units: Vec<u16> = bytes[..bytes.len() - 2]
        .chunks_exact(2)
        .map(|p| u16::from_be_bytes([p[0], p[1]]))
        .collect();
    let name = String::from_utf16(&units).context("Invalid UTF-16 Bluetooth file name")?;
    validate_name(&name)?;
    Ok(name)
}

fn name_header(name: &str) -> Result<Vec<u8>> {
    validate_name(name)?;
    let mut encoded = Vec::with_capacity(name.len() * 2 + 2);
    for unit in name.encode_utf16().chain(std::iter::once(0)) {
        encoded.extend_from_slice(&unit.to_be_bytes());
    }
    variable_header(1, &encoded)
}

/// OPP sender. A single owner holds the RFCOMM connection until DISCONNECT.
pub struct OppClient<T> {
    stream: T,
    peer_mtu: usize,
    connected: bool,
    failed: bool,
    connection: Vec<u8>,
}

impl<T: Read + Write> OppClient<T> {
    pub fn new(stream: T) -> Self {
        Self {
            stream,
            peer_mtu: 255,
            connected: false,
            failed: false,
            connection: vec![],
        }
    }

    fn exchange(&mut self, opcode: u8, data: &[u8]) -> Result<Vec<u8>> {
        ensure!(!self.failed, "Bluetooth stream requires reconnection");
        let request = if opcode == 0x80 {
            packet(opcode, data)?
        } else {
            let mut payload = self.connection.clone();
            payload.extend_from_slice(data);
            packet(opcode, &payload)?
        };
        ensure!(
            opcode == 0x80 || request.len() <= self.peer_mtu,
            "Bluetooth request exceeds negotiated packet size"
        );
        let result = (|| {
            self.stream.write_all(&request)?;
            self.stream.flush()?;
            read_packet(&mut self.stream, RECEIVE_MTU)
        })();
        if result.is_err() {
            self.failed = true;
            self.connected = false;
        }
        result
    }

    pub fn connect(&mut self) -> Result<()> {
        if self.connected {
            return Ok(());
        }
        let response = self.exchange(
            0x80,
            &[0x10, 0, (RECEIVE_MTU >> 8) as u8, RECEIVE_MTU as u8],
        )?;
        ensure!(
            response[0] == SUCCESS,
            "Bluetooth OPP CONNECT rejected ({:02X})",
            response[0]
        );
        ensure!(
            response.len() >= 7 && response[3] >> 4 == 1 && response[4] == 0,
            "Invalid Bluetooth OPP CONNECT response"
        );
        self.peer_mtu = u16::from_be_bytes([response[5], response[6]]) as usize;
        ensure!(
            self.peer_mtu >= 255,
            "Bluetooth peer packet size is too small"
        );
        // OPP has no FTP target. Echo a connection ID if the peer assigns one.
        self.connection = connection_header(&headers(&response[7..])?)?;
        self.connected = true;
        Ok(())
    }

    pub fn send_file(&mut self, path: &Path) -> Result<u64> {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .context("Invalid Bluetooth file name")?;
        let mut source = File::open(path).context("Cannot open Bluetooth source file")?;
        let bytes = source.metadata()?.len();
        self.send(name, &mut source, bytes, |_, _| {})
    }

    pub fn send<R: Read>(
        &mut self,
        name: &str,
        source: &mut R,
        bytes: u64,
        mut progress: impl FnMut(u64, u64),
    ) -> Result<u64> {
        ensure!(
            self.connected && !self.failed,
            "Bluetooth OPP is not connected"
        );
        ensure!(
            bytes <= MAX_OBJECT_BYTES,
            "Bluetooth object exceeds 32 MiB limit"
        );
        let mut metadata = name_header(name)?;
        metadata.push(0xc3);
        metadata.extend_from_slice(&(bytes as u32).to_be_bytes());
        let mut count = 0;
        let result = (|| {
            loop {
                let capacity = self
                    .peer_mtu
                    .checked_sub(6 + metadata.len() + self.connection.len())
                    .context("Bluetooth file name exceeds peer packet size")?;
                // A metadata-only initial PUT is legal; the next PUT has the
                // full packet capacity. No empty continuation loop is possible.
                let wanted = capacity.min((bytes - count) as usize);
                let mut body = vec![0; wanted];
                source
                    .read_exact(&mut body)
                    .context("Bluetooth source file ended early")?;
                let final_packet = count + wanted as u64 == bytes;
                let mut data = std::mem::take(&mut metadata);
                data.extend_from_slice(&variable_header(
                    if final_packet { 0x49 } else { 0x48 },
                    &body,
                )?);
                let response = self.exchange(if final_packet { 0x82 } else { 0x02 }, &data)?;
                ensure!(
                    response[0] == if final_packet { SUCCESS } else { CONTINUE },
                    "Bluetooth peer rejected file ({:02X})",
                    response[0]
                );
                headers(&response[3..])?;
                count += wanted as u64;
                progress(count, bytes);
                if final_packet {
                    return Ok(count);
                }
            }
        })();
        if result.is_err() {
            // A source/protocol failure after a continuation must not leave
            // stale partial data attached to the next object's PUT stream.
            if !self.failed {
                let _ = self.exchange(0xff, &[]);
            }
            self.failed = true;
            self.connected = false;
        }
        result
    }

    pub fn disconnect(&mut self) -> Result<()> {
        if !self.connected {
            return Ok(());
        }
        let response = self.exchange(0x81, &[])?;
        self.connected = false;
        ensure!(response[0] == SUCCESS, "Bluetooth OPP DISCONNECT rejected");
        Ok(())
    }
}

#[derive(Clone, Copy)]
enum SenderState {
    Connect,
    Put,
    Disconnect,
    Done,
    Failed,
}

/// Pull-based file sender for Android BluetoothSocket JNI integration. The UI
/// transports opaque packets and never needs its own OBEX implementation.
pub struct OppSenderFile {
    source: File,
    name: String,
    bytes: u64,
    count: u64,
    acknowledged: u64,
    peer_mtu: usize,
    first_put: bool,
    final_put: bool,
    pending: bool,
    state: SenderState,
    connection: Vec<u8>,
}

impl OppSenderFile {
    pub fn new(path: &Path) -> Result<Self> {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .context("Invalid Bluetooth file name")?
            .to_owned();
        Self::new_named(path, &name)
    }

    pub fn new_named(path: &Path, name: &str) -> Result<Self> {
        validate_name(name)?;
        let source = File::open(path).context("Cannot open Bluetooth source file")?;
        let bytes = source.metadata()?.len();
        ensure!(
            bytes <= MAX_OBJECT_BYTES,
            "Bluetooth object exceeds 32 MiB limit"
        );
        Ok(Self {
            source,
            name: name.to_owned(),
            bytes,
            count: 0,
            acknowledged: 0,
            peer_mtu: 255,
            first_put: true,
            final_put: false,
            pending: false,
            state: SenderState::Connect,
            connection: vec![],
        })
    }

    pub fn progress(&self) -> (u64, u64) {
        (self.acknowledged, self.bytes)
    }
    pub fn finished(&self) -> bool {
        matches!(self.state, SenderState::Done)
    }

    pub fn next_packet(&mut self) -> Result<Option<Vec<u8>>> {
        ensure!(
            !self.pending,
            "Bluetooth response must be consumed before the next packet"
        );
        let request = (|| match self.state {
            SenderState::Connect => packet(
                0x80,
                &[0x10, 0, (RECEIVE_MTU >> 8) as u8, RECEIVE_MTU as u8],
            )
            .map(Some),
            SenderState::Disconnect => packet(0x81, &self.connection).map(Some),
            SenderState::Done => Ok(None),
            SenderState::Failed => bail!("Bluetooth transfer requires reconnection"),
            SenderState::Put => {
                let mut data = if self.first_put {
                    let mut metadata = name_header(&self.name)?;
                    metadata.push(0xc3);
                    metadata.extend_from_slice(&(self.bytes as u32).to_be_bytes());
                    metadata
                } else {
                    vec![]
                };
                if !self.connection.is_empty() {
                    let mut with_connection = self.connection.clone();
                    with_connection.extend(data);
                    data = with_connection;
                }
                let capacity = self
                    .peer_mtu
                    .checked_sub(6 + data.len())
                    .context("Bluetooth file name exceeds peer packet size")?;
                let wanted = capacity.min((self.bytes - self.count) as usize);
                let mut body = vec![0; wanted];
                self.source
                    .read_exact(&mut body)
                    .context("Bluetooth source file ended early")?;
                self.count += wanted as u64;
                self.final_put = self.count == self.bytes;
                self.first_put = false;
                data.extend(variable_header(
                    if self.final_put { 0x49 } else { 0x48 },
                    &body,
                )?);
                packet(if self.final_put { 0x82 } else { 0x02 }, &data).map(Some)
            }
        })();
        match request {
            Ok(request) => {
                self.pending = request.is_some();
                Ok(request)
            }
            Err(error) => {
                self.state = SenderState::Failed;
                Err(error)
            }
        }
    }

    pub fn accept_response(&mut self, response: &[u8]) -> Result<()> {
        ensure!(self.pending, "No Bluetooth request is awaiting a response");
        let result = (|| {
            ensure!(
                (3..=RECEIVE_MTU).contains(&response.len())
                    && u16::from_be_bytes([response[1], response[2]]) as usize == response.len(),
                "Invalid Bluetooth OBEX response framing"
            );
            match self.state {
                SenderState::Connect => {
                    ensure!(
                        response[0] == SUCCESS
                            && response.len() >= 7
                            && response[3] >> 4 == 1
                            && response[4] == 0,
                        "Bluetooth OPP CONNECT rejected or malformed"
                    );
                    self.peer_mtu = u16::from_be_bytes([response[5], response[6]]) as usize;
                    ensure!(
                        self.peer_mtu >= 255,
                        "Bluetooth peer packet size is too small"
                    );
                    self.connection = connection_header(&headers(&response[7..])?)?;
                    self.state = SenderState::Put;
                }
                SenderState::Put => {
                    ensure!(
                        response[0] == if self.final_put { SUCCESS } else { CONTINUE },
                        "Bluetooth peer rejected file ({:02X})",
                        response[0]
                    );
                    headers(&response[3..])?;
                    self.acknowledged = self.count;
                    if self.final_put {
                        self.state = SenderState::Disconnect;
                    }
                }
                SenderState::Disconnect => {
                    ensure!(response[0] == SUCCESS, "Bluetooth OPP DISCONNECT rejected");
                    headers(&response[3..])?;
                    self.state = SenderState::Done;
                }
                _ => bail!("Bluetooth response arrived outside a live transfer"),
            }
            Ok(())
        })();
        self.pending = false;
        if result.is_err() {
            self.state = SenderState::Failed;
        }
        result
    }
}

struct Incoming {
    name: String,
    path: PathBuf,
    file: Option<File>,
    expected: Option<u64>,
    bytes: u64,
    digest: Sha256,
    committed: bool,
}

impl Drop for Incoming {
    fn drop(&mut self) {
        // Windows cannot unlink an open staging file.
        self.file = None;
        if !self.committed {
            let _ = fs::remove_file(&self.path);
        }
    }
}

/// Stateful OPP receiver for JNI and native host sockets. `handle_packet`
/// accepts exactly one complete bounded packet and returns its wire response.
/// Successful objects stay in a private staging directory for explicit import.
pub struct OppReceiver {
    staging: PathBuf,
    connected: bool,
    incoming: Option<Incoming>,
    received: Option<ReceivedFile>,
}

impl OppReceiver {
    pub fn new(staging: &Path) -> Result<Self> {
        fs::create_dir_all(staging).context("Cannot create Bluetooth staging directory")?;
        Ok(Self {
            staging: staging.to_owned(),
            connected: false,
            incoming: None,
            received: None,
        })
    }

    pub fn connected(&self) -> bool {
        self.connected
    }
    pub fn take_received(&mut self) -> Option<ReceivedFile> {
        self.received.take()
    }

    pub fn handle_packet(&mut self, request: &[u8]) -> Result<Vec<u8>> {
        ensure!(
            (3..=RECEIVE_MTU).contains(&request.len())
                && u16::from_be_bytes([request[1], request[2]]) as usize == request.len(),
            "Invalid Bluetooth OBEX packet framing"
        );
        match request[0] {
            0x80 => {
                ensure!(
                    !self.connected
                        && request.len() >= 7
                        && request[3] >> 4 == 1
                        && request[4] == 0,
                    "Invalid Bluetooth OPP CONNECT request"
                );
                ensure!(
                    u16::from_be_bytes([request[5], request[6]]) >= 255,
                    "Bluetooth peer packet size is too small"
                );
                ensure!(
                    !headers(&request[7..])?.iter().any(|(tag, _)| *tag == 0x46),
                    "Bluetooth OPP does not implement the FTP target"
                );
                self.connected = true;
                packet(
                    SUCCESS,
                    &[0x10, 0, (RECEIVE_MTU >> 8) as u8, RECEIVE_MTU as u8],
                )
            }
            0x81 => {
                self.incoming = None;
                self.connected = false;
                packet(SUCCESS, &[])
            }
            0xff => {
                ensure!(self.connected, "Bluetooth OPP is not connected");
                self.incoming = None;
                packet(SUCCESS, &[])
            }
            0x02 | 0x82 => {
                ensure!(self.connected, "Bluetooth OPP is not connected");
                let result = self.put(&request[3..], request[0] == 0x82);
                if result.is_err() {
                    self.incoming = None;
                }
                result
            }
            _ => packet(NOT_IMPLEMENTED, &[]),
        }
    }

    /// Sends a standard OBEX refusal and abandons only the incomplete object.
    /// Native UIs can report the associated error while keeping framing intact.
    pub fn reject_packet(&mut self) -> Vec<u8> {
        self.incoming = None;
        vec![BAD_REQUEST, 0, 3]
    }

    fn put(&mut self, data: &[u8], final_packet: bool) -> Result<Vec<u8>> {
        let fields = headers(data)?;
        let names: Vec<_> = fields.iter().filter(|(tag, _)| *tag == 1).collect();
        let sizes: Vec<_> = fields.iter().filter(|(tag, _)| *tag == 0xc3).collect();
        ensure!(
            names.len() <= 1 && sizes.len() <= 1,
            "Duplicate Bluetooth object metadata"
        );
        let name = names
            .first()
            .map(|(_, value)| decode_name(value))
            .transpose()?;
        let expected = sizes
            .first()
            .map(|(_, value)| u32::from_be_bytes((*value).try_into().unwrap()) as u64);
        ensure!(
            expected.is_none_or(|n| n <= MAX_OBJECT_BYTES),
            "Bluetooth object exceeds 32 MiB limit"
        );
        if self.incoming.is_none() {
            ensure!(
                self.received.is_none(),
                "Import or release the previous Bluetooth file before receiving another"
            );
            let name = name.context("Bluetooth PUT requires a file name")?;
            let (path, file) = self.new_stage()?;
            self.incoming = Some(Incoming {
                name,
                path,
                file: Some(file),
                expected,
                bytes: 0,
                digest: Sha256::new(),
                committed: false,
            });
        } else {
            let incoming = self.incoming.as_mut().unwrap();
            ensure!(
                name.as_ref().is_none_or(|n| *n == incoming.name),
                "Bluetooth object name changed mid-transfer"
            );
            if let Some(expected) = expected {
                ensure!(
                    incoming.expected.is_none_or(|n| n == expected),
                    "Bluetooth object size changed mid-transfer"
                );
                incoming.expected = Some(expected);
            }
        }
        let incoming = self.incoming.as_mut().unwrap();
        let mut ended = false;
        for (tag, value) in fields {
            if matches!(tag, 0x48 | 0x49) {
                ensure!(!ended, "Bluetooth object body follows End-of-Body");
                ended = tag == 0x49;
                let bytes = incoming
                    .bytes
                    .checked_add(value.len() as u64)
                    .context("Bluetooth object too large")?;
                ensure!(
                    bytes <= MAX_OBJECT_BYTES && incoming.expected.is_none_or(|n| bytes <= n),
                    "Bluetooth object body exceeds declared length or 32 MiB limit"
                );
                incoming
                    .file
                    .as_mut()
                    .unwrap()
                    .write_all(value)
                    .context("Cannot save Bluetooth object")?;
                incoming.digest.update(value);
                incoming.bytes = bytes;
            }
        }
        ensure!(
            final_packet == ended,
            "Bluetooth final PUT and End-of-Body disagree"
        );
        if !final_packet {
            return packet(CONTINUE, &[]);
        }
        ensure!(
            incoming.expected.is_none_or(|n| incoming.bytes == n),
            "Incomplete Bluetooth object"
        );
        incoming
            .file
            .as_mut()
            .unwrap()
            .sync_all()
            .context("Cannot finish Bluetooth object")?;
        incoming.committed = true;
        self.received = Some(ReceivedFile {
            name: incoming.name.clone(),
            path: incoming.path.clone(),
            bytes: incoming.bytes,
            sha256: format!("{:x}", incoming.digest.clone().finalize()),
        });
        self.incoming = None;
        packet(SUCCESS, &[])
    }

    fn new_stage(&self) -> Result<(PathBuf, File)> {
        for _ in 0..100 {
            let id = NEXT_STAGE.fetch_add(1, Ordering::Relaxed);
            let path = self
                .staging
                .join(format!("bt-{}-{id}.object", std::process::id()));
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(file) => return Ok((path, file)),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error).context("Cannot stage Bluetooth object"),
            }
        }
        bail!("Cannot allocate Bluetooth staging file")
    }
}

/// Receives one object over a host RFCOMM socket. A transport failure drops
/// the partial staging file. After completion the peer may either DISCONNECT
/// or close the connection; both return the completed object for explicit import.
pub fn receive_one<T: Read + Write>(stream: &mut T, staging: &Path) -> Result<ReceivedFile> {
    let mut receiver = OppReceiver::new(staging)?;
    for _ in 0..MAX_OBJECT_BYTES / 4 + 1024 {
        let request = read_packet(stream, RECEIVE_MTU)?;
        let response = match receiver.handle_packet(&request) {
            Ok(response) => response,
            Err(error) => {
                stream.write_all(&receiver.reject_packet())?;
                stream.flush()?;
                return Err(error);
            }
        };
        stream.write_all(&response)?;
        stream.flush()?;
        if let Some(file) = receiver.take_received() {
            // Honour the standard DISCONNECT rather than closing a successful
            // PUT before the sender can finish its session. The caller's socket
            // deadline also covers peers that leave their connection open.
            if let Ok(request) = read_packet(stream, RECEIVE_MTU) {
                let response = if request[0] == 0x81 {
                    receiver.handle_packet(&request)?
                } else {
                    receiver.reject_packet()
                };
                let _ = stream.write_all(&response);
                let _ = stream.flush();
            }
            return Ok(file);
        }
        ensure!(
            receiver.connected(),
            "Bluetooth peer ended the session without a file"
        );
    }
    bail!("Bluetooth transfer exceeded packet limit")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{self, Cursor};

    struct Stage(PathBuf);
    impl Stage {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "ph0nix-bt-test-{}-{}",
                std::process::id(),
                NEXT_STAGE.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&root).unwrap();
            Self(root)
        }
    }
    impl Drop for Stage {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    struct Loopback {
        receiver: OppReceiver,
        response: Cursor<Vec<u8>>,
        requests: usize,
    }
    impl Read for Loopback {
        fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
            self.response.read(bytes)
        }
    }
    impl Write for Loopback {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.requests += 1;
            let response = self
                .receiver
                .handle_packet(bytes)
                .map_err(io::Error::other)?;
            self.response = Cursor::new(response);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn object_push_streams_unicode_multiframe_data_and_verifies_digest() {
        let stage = Stage::new();
        let link = Loopback {
            receiver: OppReceiver::new(&stage.0).unwrap(),
            response: Cursor::new(vec![]),
            requests: 0,
        };
        let mut client = OppClient::new(link);
        client.connect().unwrap();
        let data: Vec<u8> = (0..8765).map(|i| (i % 253) as u8).collect();
        let mut progress = Vec::new();
        assert_eq!(
            client
                .send(
                    "Игра.jar",
                    &mut Cursor::new(&data),
                    data.len() as u64,
                    |n, _| progress.push(n)
                )
                .unwrap(),
            data.len() as u64
        );
        let file = client.stream.receiver.take_received().unwrap();
        assert_eq!(file.name, "Игра.jar");
        assert_eq!(fs::read(file.path).unwrap(), data);
        assert_eq!(file.sha256, format!("{:x}", Sha256::digest(&data)));
        assert_eq!(progress.last(), Some(&(data.len() as u64)));
        assert!(client.stream.requests > 8);
        client.disconnect().unwrap();
    }

    #[test]
    fn jni_packet_sender_obeys_request_response_order_and_preserves_original_name() {
        let stage = Stage::new();
        let source = stage.0.join("private-staging.object");
        let data = vec![0x7a; 4567];
        fs::write(&source, &data).unwrap();
        let mut sender = OppSenderFile::new_named(&source, "Aero.jar").unwrap();
        let mut receiver = OppReceiver::new(&stage.0).unwrap();
        let mut count = 0;
        while let Some(request) = sender.next_packet().unwrap() {
            assert!(sender.next_packet().is_err());
            let response = receiver.handle_packet(&request).unwrap();
            sender.accept_response(&response).unwrap();
            assert!(sender.accept_response(&response).is_err());
            count += 1;
        }
        assert!(count > 4);
        assert!(sender.finished());
        assert_eq!(sender.progress(), (data.len() as u64, data.len() as u64));
        let received = receiver.take_received().unwrap();
        assert_eq!(received.name, "Aero.jar");
        assert_eq!(fs::read(received.path).unwrap(), data);
    }

    #[test]
    fn jni_sender_echoes_connection_id_and_respects_minimum_peer_mtu() {
        let stage = Stage::new();
        let source = stage.0.join("a.dat");
        fs::write(&source, vec![1; 900]).unwrap();
        let mut sender = OppSenderFile::new(&source).unwrap();
        assert_eq!(
            sender.next_packet().unwrap().unwrap(),
            [0x80, 0, 7, 0x10, 0, 4, 0]
        );
        sender
            .accept_response(&[0xa0, 0, 12, 0x10, 0, 0, 255, 0xcb, 1, 2, 3, 4])
            .unwrap();
        loop {
            let request = sender.next_packet().unwrap().unwrap();
            assert!(request.len() <= 255);
            assert_eq!(&request[3..8], &[0xcb, 1, 2, 3, 4]);
            let opcode = request[0];
            sender
                .accept_response(&[if opcode == 2 { CONTINUE } else { SUCCESS }, 0, 3])
                .unwrap();
            if opcode == 0x81 {
                break;
            }
        }
        assert!(sender.finished());
        assert_eq!(sender.progress(), (900, 900));
    }

    #[test]
    fn portable_receiver_handles_fragmented_native_stream_and_disconnect() {
        use std::net::{TcpListener, TcpStream};
        use std::time::Duration;
        struct Fragmented(TcpStream);
        impl Read for Fragmented {
            fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
                let limit = bytes.len().min(7);
                self.0.read(&mut bytes[..limit])
            }
        }
        impl Write for Fragmented {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                self.0.write(&bytes[..bytes.len().min(11)])
            }
            fn flush(&mut self) -> io::Result<()> {
                self.0.flush()
            }
        }
        let stage = Stage::new();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let staging = stage.0.clone();
        let receiving = std::thread::spawn(move || {
            let (socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            socket
                .set_write_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            receive_one(&mut Fragmented(socket), &staging).unwrap()
        });
        let socket = TcpStream::connect(address).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        socket
            .set_write_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let mut client = OppClient::new(Fragmented(socket));
        client.connect().unwrap();
        let bytes: Vec<u8> = (0..4097).map(|i| (i % 251) as u8).collect();
        client
            .send(
                "picture.png",
                &mut Cursor::new(&bytes),
                bytes.len() as u64,
                |_, _| {},
            )
            .unwrap();
        client.disconnect().unwrap();
        let received = receiving.join().unwrap();
        assert_eq!(received.name, "picture.png");
        assert_eq!(fs::read(received.path).unwrap(), bytes);
    }

    #[test]
    fn malformed_names_and_truncated_packets_do_not_create_files() {
        let stage = Stage::new();
        let mut receiver = OppReceiver::new(&stage.0).unwrap();
        receiver
            .handle_packet(&packet(0x80, &[0x10, 0, 4, 0]).unwrap())
            .unwrap();
        for name in ["../out", "a\\b", "a:b", "..", "a\0b"] {
            let mut encoded = Vec::new();
            for unit in name.encode_utf16().chain([0]) {
                encoded.extend(unit.to_be_bytes());
            }
            let mut data = variable_header(1, &encoded).unwrap();
            data.extend(variable_header(0x49, b"x").unwrap());
            assert!(receiver
                .handle_packet(&packet(0x82, &data).unwrap())
                .is_err());
        }
        assert!(receiver.handle_packet(&[0x82, 0, 8, 1, 0, 5]).is_err());
        assert_eq!(fs::read_dir(&stage.0).unwrap().count(), 0);
    }

    #[test]
    fn early_source_failure_aborts_and_removes_partial_object() {
        let stage = Stage::new();
        let link = Loopback {
            receiver: OppReceiver::new(&stage.0).unwrap(),
            response: Cursor::new(vec![]),
            requests: 0,
        };
        let mut client = OppClient::new(link);
        client.connect().unwrap();
        assert!(client
            .send("a.jar", &mut Cursor::new(vec![0; 1100]), 2000, |_, _| {})
            .is_err());
        assert_eq!(fs::read_dir(&stage.0).unwrap().count(), 0);
        assert!(client
            .send("b.jar", &mut Cursor::new(vec![]), 0, |_, _| {})
            .is_err());
    }

    #[test]
    fn abort_and_drop_remove_incomplete_received_file() {
        let stage = Stage::new();
        {
            let mut receiver = OppReceiver::new(&stage.0).unwrap();
            receiver
                .handle_packet(&packet(0x80, &[0x10, 0, 4, 0]).unwrap())
                .unwrap();
            let mut data = name_header("x.txt").unwrap();
            data.extend(variable_header(0x48, b"partial").unwrap());
            receiver
                .handle_packet(&packet(0x02, &data).unwrap())
                .unwrap();
            assert_eq!(fs::read_dir(&stage.0).unwrap().count(), 1);
            receiver.handle_packet(&[0xff, 0, 3]).unwrap();
            assert_eq!(fs::read_dir(&stage.0).unwrap().count(), 0);
            receiver
                .handle_packet(&packet(0x02, &data).unwrap())
                .unwrap();
        }
        assert_eq!(fs::read_dir(&stage.0).unwrap().count(), 0);
    }

    #[test]
    fn invalid_final_size_does_not_commit_and_empty_files_work() {
        let stage = Stage::new();
        let mut receiver = OppReceiver::new(&stage.0).unwrap();
        receiver
            .handle_packet(&packet(0x80, &[0x10, 0, 4, 0]).unwrap())
            .unwrap();
        let mut data = name_header("bad.txt").unwrap();
        data.extend_from_slice(&[0xc3, 0, 0, 0, 8]);
        data.extend(variable_header(0x49, b"short").unwrap());
        assert!(receiver
            .handle_packet(&packet(0x82, &data).unwrap())
            .is_err());
        assert_eq!(fs::read_dir(&stage.0).unwrap().count(), 0);
        let mut data = name_header("empty.txt").unwrap();
        data.extend(variable_header(0x49, b"").unwrap());
        assert_eq!(
            receiver
                .handle_packet(&packet(0x82, &data).unwrap())
                .unwrap(),
            [SUCCESS, 0, 3]
        );
        assert_eq!(receiver.take_received().unwrap().bytes, 0);
    }

    #[test]
    fn packet_bounds_and_ftp_target_are_rejected() {
        assert!(read_packet(&mut Cursor::new([0x82, 0xff, 0xff]), RECEIVE_MTU).is_err());
        assert!(read_packet(&mut Cursor::new([0x82, 0, 2]), RECEIVE_MTU).is_err());
        let stage = Stage::new();
        let mut receiver = OppReceiver::new(&stage.0).unwrap();
        let mut data = vec![0x10, 0, 4, 0];
        data.extend(variable_header(0x46, &[0; 16]).unwrap());
        assert!(receiver
            .handle_packet(&packet(0x80, &data).unwrap())
            .is_err());
        assert!(!receiver.connected());
    }
}
