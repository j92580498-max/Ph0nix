// SPDX-License-Identifier: GPL-2.0-or-later
//! Bounded OBEX file-transfer client. The handset's firmware serves every object.
use super::{FileEntry, MAX_FILE_BYTES};
use anyhow::{bail, ensure, Context, Result};
use std::io::{self, Read, Write};
use std::time::{Duration, Instant};

const TARGET: &[u8] = &[
    0xf9, 0xec, 0x7b, 0xc4, 0x95, 0x3c, 0x11, 0xd2, 0x98, 0x4e, 0x52, 0x54, 0, 0xdc, 0x9e, 9,
];
const RECEIVE_MAX: usize = 4096;
const MAX_LIST_BYTES: usize = 4 * 1024 * 1024;

pub fn validate_path(parts: &[String]) -> Result<()> {
    ensure!(parts.len() <= 32, "Phone path is too deep");
    for part in parts {
        ensure!(
            !part.is_empty()
                && part != "."
                && part != ".."
                && !part.chars().any(|c| matches!(c, '/' | '\\' | '\0'))
                && part.encode_utf16().count() <= 255,
            "Invalid phone path component"
        );
    }
    Ok(())
}

fn header(tag: u8, value: &[u8]) -> Result<Vec<u8>> {
    let mut result = vec![tag];
    if tag & 0xc0 < 0x80 {
        ensure!(
            value.len() + 3 <= u16::MAX as usize,
            "OBEX header exceeds packet limit"
        );
        result.extend_from_slice(&((value.len() + 3) as u16).to_be_bytes());
    } else {
        ensure!(
            value.len() == if tag & 0xc0 == 0x80 { 1 } else { 4 },
            "Invalid OBEX fixed header"
        );
    }
    result.extend_from_slice(value);
    Ok(result)
}

fn name_header(name: &str) -> Result<Vec<u8>> {
    if name.is_empty() {
        return header(1, &[]);
    }
    validate_path(&[name.to_owned()])?;
    let mut value = Vec::with_capacity(name.len() * 2 + 2);
    for word in name.encode_utf16().chain(Some(0)) {
        value.extend_from_slice(&word.to_be_bytes());
    }
    header(1, &value)
}

fn parse_headers(mut bytes: &[u8]) -> Result<Vec<(u8, &[u8])>> {
    let mut result = Vec::new();
    while !bytes.is_empty() {
        let tag = bytes[0];
        let (size, start) = match tag & 0xc0 {
            0 | 0x40 => {
                ensure!(bytes.len() >= 3, "Truncated OBEX header");
                (u16::from_be_bytes([bytes[1], bytes[2]]) as usize, 3)
            }
            0x80 => (2, 1),
            _ => (5, 1),
        };
        ensure!(
            size >= start && size <= bytes.len(),
            "Invalid OBEX header length"
        );
        result.push((tag, &bytes[start..size]));
        bytes = &bytes[size..];
    }
    Ok(result)
}

fn remote_error(status: u8) -> anyhow::Error {
    anyhow::anyhow!(
        "{} (OBEX {status:02X})",
        match status {
            0xc0 => "Phone rejected the request",
            0xc1 => "Authorize the transfer on the phone",
            0xc3 => "Phone denied access",
            0xc4 => "Phone file or folder not found",
            0xcd => "Phone storage is full",
            _ => "Phone reported a file-transfer error",
        }
    )
}

#[derive(PartialEq)]
enum AtReply {
    Ok,
    Connect,
    Refused,
}

pub struct ObexClient<T: Read + Write> {
    stream: T,
    peer_max: usize,
    connection: Vec<u8>,
    connected: bool,
    failure: Option<String>,
    timeout: Duration,
}
impl<T: Read + Write> ObexClient<T> {
    pub fn new(stream: T) -> Self {
        Self {
            stream,
            peer_max: 255,
            connection: Vec::new(),
            connected: false,
            failure: None,
            timeout: Duration::from_secs(15),
        }
    }
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }
    pub fn is_connected(&self) -> bool {
        self.connected && !self.failed()
    }
    pub fn failed(&self) -> bool {
        self.failure.is_some()
    }
    fn check_stream(&self) -> Result<()> {
        if let Some(reason) = &self.failure {
            bail!("Restart K750i to reopen the file-transfer stream: {reason}");
        }
        Ok(())
    }
    fn mark_failed(&mut self, error: &anyhow::Error) {
        self.connected = false;
        self.failure.get_or_insert_with(|| format!("{error:#}"));
    }
    fn exact(&mut self, bytes: &mut [u8], deadline: Instant) -> Result<()> {
        let mut count = 0;
        while count < bytes.len() {
            if Instant::now() >= deadline {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "Phone receive deadline exceeded").into());
            }
            match self.stream.read(&mut bytes[count..]) {
                Ok(0) => bail!("Phone file-transfer stream disconnected"),
                Ok(size) => count += size,
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock
                            | std::io::ErrorKind::TimedOut
                            | std::io::ErrorKind::Interrupted
                    ) => {}
                Err(e) => return Err(e.into()),
            }
        }
        Ok(())
    }
    fn write(&mut self, bytes: &[u8], deadline: Instant) -> Result<()> {
        let mut count = 0;
        while count < bytes.len() {
            ensure!(Instant::now() < deadline, "Phone send deadline exceeded");
            match self.stream.write(&bytes[count..]) {
                Ok(0) => bail!("Phone file-transfer stream disconnected"),
                Ok(size) => count += size,
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock
                            | std::io::ErrorKind::TimedOut
                            | std::io::ErrorKind::Interrupted
                    ) => {}
                Err(e) => return Err(e.into()),
            }
        }
        self.stream.flush()?;
        Ok(())
    }
    fn exchange(&mut self, opcode: u8, data: &[u8]) -> Result<(u8, Vec<u8>)> {
        self.check_stream()?;
        ensure!(
            data.len() + 3 <= if opcode == 0x80 { 65535 } else { self.peer_max },
            "OBEX request exceeds phone packet size"
        );
        let deadline = Instant::now() + self.timeout;
        let mut request = vec![opcode];
        request.extend_from_slice(&((data.len() + 3) as u16).to_be_bytes());
        request.extend_from_slice(data);
        let result = (|| {
            self.write(&request, deadline)?;
            let mut prefix = [0; 3];
            self.exact(&mut prefix, deadline)?;
            let length = u16::from_be_bytes([prefix[1], prefix[2]]) as usize;
            ensure!(
                (3..=RECEIVE_MAX).contains(&length),
                "Invalid OBEX response length"
            );
            let mut body = vec![0; length - 3];
            self.exact(&mut body, deadline)?;
            Ok((prefix[0], body))
        })();
        if let Err(error) = &result {
            self.mark_failed(error);
        }
        result
    }
    fn read_at_reply(&mut self, deadline: Instant, handover: bool) -> Result<AtReply> {
        let mut line = Vec::new();
        let mut saw_ok = false;
        for received in 0..4096 {
            let mut byte = [0];
            self.exact(&mut byte, deadline).with_context(|| {
                format!("AT reply incomplete ({received} bytes, OK received: {saw_ok})")
            })?;
            line.push(byte[0]);
            if line.ends_with(b"\r\n") {
                let text = line.trim_ascii();
                match text {
                    b"OK" if !handover => return Ok(AtReply::Ok),
                    b"OK" => saw_ok = true,
                    b"CONNECT" => return Ok(AtReply::Connect),
                    b"ERROR" | b"NO CARRIER" | b"BUSY" => return Ok(AtReply::Refused),
                    _ if text.starts_with(b"+CME ERROR:") => return Ok(AtReply::Refused),
                    _ => {}
                }
                line.clear();
            }
        }
        bail!("Invalid AT response")
    }
    fn handover(&mut self, command: &str) -> Result<bool> {
        let deadline = Instant::now() + self.timeout;
        self.write(command.as_bytes(), deadline)
            .and_then(|()| self.read_at_reply(deadline, true))
            .with_context(|| format!("{} handover", command.trim_end()))
            .map(|reply| reply == AtReply::Connect)
    }
    /// A ready UART does not imply that the handset's AT interpreter is running.
    pub fn connect_when_ready(&mut self) -> Result<()> {
        self.check_stream()?;
        if !self.is_connected() {
            // A plain AT command cannot switch protocols. It is safe to retry if
            // the handset has not replied yet, including during first boot.
            let deadline = Instant::now() + self.timeout.min(Duration::from_secs(3));
            let result = self.write(b"AT\r", deadline)
                .and_then(|()| self.read_at_reply(deadline, false));
            match result {
                Ok(AtReply::Ok) => {}
                Ok(AtReply::Refused) => bail!("Phone file service is not ready; finish setup on K750i and retry"),
                Err(error) if error.downcast_ref::<io::Error>()
                    .is_some_and(|error| error.kind() == io::ErrorKind::TimedOut) => {
                    bail!("No reply from K750i yet; finish setup, wait for the main screen and retry");
                }
                Ok(AtReply::Connect) => {
                    let error = anyhow::anyhow!("Unexpected transition to binary file-transfer mode");
                    self.mark_failed(&error);
                    return self.check_stream();
                }
                Err(error) => {
                    self.mark_failed(&error.context("AT readiness check"));
                    return self.check_stream();
                }
            }
        }
        self.connect(false)
    }
    pub fn connect(&mut self, binary: bool) -> Result<()> {
        self.check_stream()?;
        if self.is_connected() {
            return Ok(());
        }
        if !binary {
            let handover = self.handover("AT+CPROT=0\r").and_then(|accepted| {
                if accepted {
                    Ok(true)
                } else {
                    self.handover("AT*EOBEX\r")
                }
            });
            match handover {
                // Give the phone time to switch its UART parser, as the Python host does.
                Ok(true) => std::thread::sleep(Duration::from_millis(25)),
                // Both commands ended in an explicit AT error, so it is safe to try again.
                Ok(false) => bail!("Phone did not enter file-transfer mode; finish any handset dialog and retry"),
                Err(error) => {
                    let error = error.context("AT file-transfer handover");
                    self.mark_failed(&error);
                    return self.check_stream();
                }
            }
        }
        if let Err(error) = self.connect_binary() {
            self.mark_failed(&error);
            return self.check_stream();
        }
        Ok(())
    }
    fn connect_binary(&mut self) -> Result<()> {
        let mut data = vec![0x10, 0];
        data.extend_from_slice(&(RECEIVE_MAX as u16).to_be_bytes());
        data.extend(header(0x46, TARGET)?);
        let (status, response) = self.exchange(0x80, &data)?;
        if status != 0xa0 {
            return Err(remote_error(status).context("OBEX CONNECT"));
        }
        ensure!(
            response.len() >= 4 && response[0] >> 4 == 1 && response[1] == 0,
            "Invalid OBEX CONNECT response"
        );
        self.peer_max = u16::from_be_bytes([response[2], response[3]]) as usize;
        ensure!(self.peer_max >= 255, "Phone packet size is too small");
        let fields = parse_headers(&response[4..])?;
        ensure!(
            fields
                .iter()
                .any(|(tag, value)| *tag == 0x4a && *value == TARGET),
            "Unexpected phone file-service identity"
        );
        let id = fields
            .iter()
            .find(|(tag, _)| *tag == 0xcb)
            .context("Phone connection ID is missing")?
            .1;
        self.connection = header(0xcb, id)?;
        self.connected = true;
        Ok(())
    }
    fn set_path(&mut self, path: &[String]) -> Result<()> {
        ensure!(
            self.is_connected(),
            "Phone file-transfer session is disconnected"
        );
        validate_path(path)?;
        for part in std::iter::once("").chain(path.iter().map(String::as_str)) {
            let mut request = vec![2, 0];
            request.extend_from_slice(&self.connection);
            request.extend(name_header(part)?);
            let (status, _) = self.exchange(0x85, &request)?;
            if status != 0xa0 {
                return Err(remote_error(status).context(format!("OBEX SETPATH {part:?}")));
            }
        }
        Ok(())
    }
    fn get_object(
        &mut self,
        mut metadata: Vec<u8>,
        destination: &mut impl Write,
        max: usize,
        mut progress: impl FnMut(u64, Option<u64>),
    ) -> Result<u64> {
        let mut total = None;
        let mut count = 0u64;
        for _ in 0..1048576 {
            let mut request = self.connection.clone();
            request.append(&mut metadata);
            let (status, response) = self.exchange(0x83, &request)?;
            if status != 0x90 && status != 0xa0 {
                return Err(remote_error(status).context("OBEX GET"));
            }
            let mut end = false;
            for (tag, value) in parse_headers(&response)? {
                if tag == 0xc3 {
                    total = Some(u32::from_be_bytes(value.try_into()?) as u64);
                    ensure!(
                        total.unwrap() <= max as u64,
                        "Phone file exceeds transfer limit"
                    );
                }
                if tag == 0x48 || tag == 0x49 {
                    count += value.len() as u64;
                    ensure!(count <= max as u64, "Phone object exceeds transfer limit");
                    destination.write_all(value)?;
                    end |= tag == 0x49;
                }
            }
            progress(count, total);
            if status == 0xa0 {
                ensure!(
                    end && total.is_none_or(|n| n == count),
                    "Phone completed an incomplete object"
                );
                return Ok(count);
            }
        }
        let error = anyhow::anyhow!("Too many OBEX continuation packets");
        self.mark_failed(&error);
        Err(error)
    }
    pub fn list(&mut self, path: &[String]) -> Result<Vec<FileEntry>> {
        self.set_path(path)?;
        let mut output = Vec::new();
        self.get_object(
            header(0x42, b"x-obex/folder-listing\0")?,
            &mut output,
            MAX_LIST_BYTES,
            |_, _| {},
        )?;
        parse_listing(&output)
    }
    pub fn get(
        &mut self,
        path: &[String],
        destination: &mut impl Write,
        progress: impl FnMut(u64, Option<u64>),
    ) -> Result<u64> {
        validate_path(path)?;
        let (name, parent) = path.split_last().context("Phone file name required")?;
        self.set_path(parent)?;
        self.get_object(name_header(name)?, destination, MAX_FILE_BYTES, progress)
    }
    pub fn put(
        &mut self,
        path: &[String],
        source: &mut impl Read,
        size: u64,
        mut progress: impl FnMut(u64, Option<u64>),
    ) -> Result<u64> {
        validate_path(path)?;
        ensure!(
            size <= MAX_FILE_BYTES as u64,
            "File exceeds 32 MiB transfer limit"
        );
        let (name, parent) = path.split_last().context("Phone file name required")?;
        self.set_path(parent)?;
        let mut metadata = name_header(name)?;
        metadata.extend(header(0xc3, &(size as u32).to_be_bytes())?);
        let mut count = 0;
        loop {
            let overhead = 3 + self.connection.len() + metadata.len() + 3;
            ensure!(
                self.peer_max > overhead || count == size,
                "File name exceeds phone packet size"
            );
            let capacity = self
                .peer_max
                .saturating_sub(overhead)
                .min((size - count) as usize);
            let mut data = vec![0; capacity];
            source.read_exact(&mut data)?;
            let final_packet = count + data.len() as u64 == size;
            let mut request = self.connection.clone();
            request.append(&mut metadata);
            request.extend(header(if final_packet { 0x49 } else { 0x48 }, &data)?);
            let (status, _) = self.exchange(if final_packet { 0x82 } else { 0x02 }, &request)?;
            if status != if final_packet { 0xa0 } else { 0x90 } {
                return Err(remote_error(status));
            }
            count += data.len() as u64;
            progress(count, Some(size));
            if final_packet {
                return Ok(count);
            }
        }
    }
    pub fn delete(&mut self, path: &[String]) -> Result<()> {
        validate_path(path)?;
        let (name, parent) = path.split_last().context("Phone file name required")?;
        self.set_path(parent)?;
        let mut request = self.connection.clone();
        request.extend(name_header(name)?);
        let (status, _) = self.exchange(0x82, &request)?;
        if status != 0xa0 {
            return Err(remote_error(status));
        }
        Ok(())
    }
    pub fn mkdir(&mut self, path: &[String]) -> Result<()> {
        validate_path(path)?;
        let (name, parent) = path.split_last().context("Phone folder name required")?;
        self.set_path(parent)?;
        let mut request = vec![0, 0];
        request.extend_from_slice(&self.connection);
        request.extend(name_header(name)?);
        let (status, _) = self.exchange(0x85, &request)?;
        if status != 0xa0 {
            return Err(remote_error(status));
        }
        Ok(())
    }
    pub fn disconnect(&mut self) -> Result<()> {
        if !self.is_connected() {
            return Ok(());
        }
        let request = self.connection.clone();
        let (status, _) = self.exchange(0x81, &request)?;
        self.connected = false;
        if status != 0xa0 {
            let error = remote_error(status).context("OBEX DISCONNECT");
            self.mark_failed(&error);
            return Err(error);
        }
        Ok(())
    }
}

fn parse_listing(bytes: &[u8]) -> Result<Vec<FileEntry>> {
    use quick_xml::{events::Event, Reader};
    let mut reader = Reader::from_reader(bytes);
    reader.config_mut().trim_text(true);
    let mut entries = Vec::new();
    let mut root = false;
    let mut closed = false;
    let mut depth = 0;
    loop {
        match reader.read_event()? {
            Event::DocType(_) => {} // Stock listings include the OBEX DTD; external entities are never resolved.
            Event::Start(node) => {
                if depth == 0 {
                    ensure!(
                        !root && node.name().as_ref() == b"folder-listing",
                        "Invalid phone directory listing"
                    );
                    root = true;
                } else if depth == 1 {
                    if let Some(entry) = listing_entry(&node, &reader)? {
                        entries.push(entry);
                    }
                }
                depth += 1;
                ensure!(depth <= 3, "Phone listing is too deeply nested");
            }
            Event::Empty(node) => {
                if depth == 0 {
                    ensure!(
                        !root && node.name().as_ref() == b"folder-listing",
                        "Invalid phone directory listing"
                    );
                    root = true;
                    closed = true;
                } else if depth == 1 {
                    if let Some(entry) = listing_entry(&node, &reader)? {
                        entries.push(entry);
                    }
                }
            }
            Event::End(node) => {
                ensure!(depth > 0, "Invalid phone listing close");
                depth -= 1;
                if depth == 0 {
                    ensure!(
                        node.name().as_ref() == b"folder-listing",
                        "Invalid phone listing root"
                    );
                    closed = true;
                }
            }
            Event::Eof => break,
            _ => {}
        }
        ensure!(
            entries.len() <= 16384,
            "Phone directory has too many entries"
        );
    }
    ensure!(
        root && closed && depth == 0,
        "Incomplete phone directory listing"
    );
    entries.sort_by(|a, b| {
        b.directory
            .cmp(&a.directory)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Ok(entries)
}
fn listing_entry(
    node: &quick_xml::events::BytesStart<'_>,
    reader: &quick_xml::Reader<&[u8]>,
) -> Result<Option<FileEntry>> {
    let tag = node.name();
    if tag.as_ref() != b"file" && tag.as_ref() != b"folder" {
        return Ok(None);
    }
    let mut entry = FileEntry {
        name: String::new(),
        directory: tag.as_ref() == b"folder",
        size: None,
        modified: None,
    };
    for attribute in node.attributes() {
        let attribute = attribute?;
        let value = attribute
            .decoded_and_normalized_value(quick_xml::XmlVersion::Implicit1_0, reader.decoder())?;
        match attribute.key.as_ref() {
            b"name" => entry.name = value.into_owned(),
            b"size" => entry.size = Some(value.parse()?),
            b"modified" => entry.modified = Some(value.into_owned()),
            _ => {}
        }
    }
    validate_path(&[entry.name.clone()])?;
    Ok(Some(entry))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    #[test]
    fn paths_and_listings_are_bounded() {
        assert!(validate_path(&["..".into()]).is_err());
        assert!(validate_path(&["a/b".into()]).is_err());
        assert!(validate_path(&["🦊".repeat(128)]).is_err());
        let entries=parse_listing(br#"<?xml version="1.0"?><!DOCTYPE folder-listing SYSTEM "obex-folder-listing.dtd"><folder-listing version="1.0"><file name="a&amp;b.txt" size="3"/><folder name="Music"/></folder-listing>"#).unwrap();
        assert_eq!(entries[0].name, "Music");
        assert_eq!(entries[1].name, "a&b.txt");
        assert!(parse_listing(b"<folder-listing><file name='../bad'/></folder-listing>").is_err());
        assert!(parse_listing(b"<folder-listing>").is_err());
        assert!(parse_headers(&[0x48, 0, 2]).is_err());
    }
    struct Script {
        reads: Cursor<Vec<u8>>,
        writes: Vec<u8>,
    }
    impl Read for Script {
        fn read(&mut self, b: &mut [u8]) -> std::io::Result<usize> {
            self.reads.read(b)
        }
    }
    impl Write for Script {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.writes.extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    fn packet(status: u8, body: &[u8]) -> Vec<u8> {
        let mut p = vec![status];
        p.extend_from_slice(&((body.len() + 3) as u16).to_be_bytes());
        p.extend_from_slice(body);
        p
    }
    fn accept_connect() -> Vec<u8> {
        let mut connect = vec![0x10, 0, 1, 0];
        connect.extend(header(0x4a, TARGET).unwrap());
        connect.extend(header(0xcb, &[1, 2, 3, 4]).unwrap());
        packet(0xa0, &connect)
    }

    #[test]
    fn both_at_refusals_allow_another_handover() {
        let mut read = b"\r\nERROR\r\n\r\nBUSY\r\n\r\nCONNECT\r\n".to_vec();
        read.extend(accept_connect());
        let mut client = ObexClient::new(Script {
            reads: Cursor::new(read),
            writes: Vec::new(),
        });
        let error = client.connect(false).unwrap_err();
        assert!(error.to_string().contains("finish any handset dialog"));
        assert!(!client.failed());
        client.disconnect().unwrap();
        assert_eq!(client.stream.writes, b"AT+CPROT=0\rAT*EOBEX\r");
        client.connect(false).unwrap();
        assert!(client.is_connected());
    }

    #[test]
    fn readiness_check_never_starts_handover_until_at_responds() {
        let mut read = b"\r\nBUSY\r\n\r\nOK\r\n\r\nCONNECT\r\n".to_vec();
        read.extend(accept_connect());
        let mut client = ObexClient::new(Script {
            reads: Cursor::new(read),
            writes: Vec::new(),
        });
        assert!(client.connect_when_ready().unwrap_err().to_string().contains("not ready"));
        assert!(!client.failed());
        assert_eq!(client.stream.writes, b"AT\r");
        client.connect_when_ready().unwrap();
        assert!(client.stream.writes.starts_with(b"AT\rAT\rAT+CPROT=0\r\x80"));
        assert!(client.is_connected());
    }

    #[test]
    fn invalid_binary_connect_preserves_error_and_blocks_another_handover() {
        let mut read = b"\r\nCONNECT\r\n".to_vec();
        read.extend(packet(0xa0, &[]));
        let mut client = ObexClient::new(Script {
            reads: Cursor::new(read),
            writes: Vec::new(),
        });
        let error = client.connect(false).unwrap_err().to_string();
        assert!(error.contains("Restart K750i"));
        assert!(error.contains("Invalid OBEX CONNECT response"));
        assert!(client.failed());
        let bytes_written = client.stream.writes.len();
        assert_eq!(client.connect(false).unwrap_err().to_string(), error);
        assert_eq!(client.stream.writes.len(), bytes_written);
    }

    #[test]
    fn real_protocol_get_and_put_packets_complete() {
        let mut read = accept_connect();
        read.extend(packet(0xa0, &[]));
        let mut body = header(0xc3, &3u32.to_be_bytes()).unwrap();
        body.extend(header(0x49, b"abc").unwrap());
        read.extend(packet(0xa0, &body));
        read.extend(packet(0xa0, &[]));
        read.extend(packet(0xa0, &[]));
        let mut client = ObexClient::new(Script {
            reads: Cursor::new(read),
            writes: Vec::new(),
        });
        client.connect(true).unwrap();
        let mut result = Vec::new();
        assert_eq!(
            client
                .get(&["x.txt".into()], &mut result, |_, _| {})
                .unwrap(),
            3
        );
        assert_eq!(result, b"abc");
        assert_eq!(
            client
                .put(&["x.txt".into()], &mut Cursor::new(b"abc"), 3, |_, _| {})
                .unwrap(),
            3
        );
        assert!(client
            .stream
            .writes
            .windows(6)
            .any(|w| w == [0x49, 0, 6, b'a', b'b', b'c']));
    }
    #[test]
    fn incomplete_download_fails_without_silent_success() {
        let mut client = ObexClient::new(Script {
            reads: Cursor::new(packet(0xa0, &header(0x48, b"abc").unwrap())),
            writes: Vec::new(),
        });
        client.connected = true;
        client.peer_max = 1024;
        assert!(client
            .get_object(Vec::new(), &mut Vec::new(), 10, |_, _| {})
            .is_err());
    }
}
