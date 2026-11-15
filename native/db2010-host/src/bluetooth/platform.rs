// SPDX-License-Identifier: GPL-2.0-or-later
//! Optional desktop host Bluetooth adapters. Android supplies BluetoothSocket.
use anyhow::{ensure, Result};

pub fn parse_address(address: &str) -> Result<[u8; 6]> {
    let parts: Vec<&str> = address.split(':').collect();
    ensure!(
        parts.len() == 6
            && parts
                .iter()
                .all(|p| p.len() == 2 && p.bytes().all(|b| b.is_ascii_hexdigit())),
        "Enter a Bluetooth address such as 12:34:56:78:9A:BC"
    );
    let mut result = [0; 6];
    for (target, part) in result.iter_mut().zip(parts) {
        *target = u8::from_str_radix(part, 16)?;
    }
    Ok(result)
}

#[cfg(any(target_os = "linux", windows))]
fn format_address(bytes: [u8; 6]) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(":")
}

/// A standard SDP Object Push record, including L2CAP/RFCOMM/OBEX protocol
/// descriptors, profile version, public browse group and any-object format.
#[cfg(target_os = "linux")]
fn opp_record(channel: u8) -> Vec<u8> {
    let mut attributes = vec![
        0x09, 0x00, 0x01, 0x35, 0x03, 0x19, 0x11, 0x05, 0x09, 0x00, 0x04, 0x35, 0x11, 0x35, 0x03,
        0x19, 0x01, 0x00, 0x35, 0x05, 0x19, 0x00, 0x03, 0x08, channel, 0x35, 0x03, 0x19, 0x00,
        0x08, 0x09, 0x00, 0x05, 0x35, 0x03, 0x19, 0x10, 0x02, 0x09, 0x00, 0x09, 0x35, 0x08, 0x35,
        0x06, 0x19, 0x11, 0x05, 0x09, 0x01, 0x00, 0x09, 0x01, 0x00, 0x25, 0x06, b'p', b'h', b'0',
        b'n', b'i', b'x', 0x09, 0x03, 0x03, 0x35, 0x02, 0x08, 0xff,
    ];
    let mut record = vec![0x35, attributes.len() as u8];
    record.append(&mut attributes);
    record
}

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::{RfcommListener, RfcommStream};
#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::{RfcommListener, RfcommStream};

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bluetooth_address_is_validated_without_shell_commands() {
        assert_eq!(
            parse_address("12:34:56:78:9a:BC").unwrap(),
            [0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc]
        );
        for value in [
            "",
            "1234:56:78:9A:BC",
            "12:34:56:78:9G:BC",
            "12:34:56:78:9A:BC;echo",
        ] {
            assert!(parse_address(value).is_err());
        }
    }
}
