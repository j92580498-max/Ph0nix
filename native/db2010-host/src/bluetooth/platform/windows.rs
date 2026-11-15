// SPDX-License-Identifier: GPL-2.0-or-later
use super::{format_address, parse_address};
use anyhow::{ensure, Context, Result};
use std::io::{self, Read, Write};
use std::mem::size_of;
use std::ptr;
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use windows_sys::core::GUID;
use windows_sys::Win32::Devices::Bluetooth::{
    AF_BTH, BTHPROTO_RFCOMM, NS_BTH, SOCKADDR_BTH, SOL_RFCOMM, SO_BTH_AUTHENTICATE, SO_BTH_ENCRYPT,
};
use windows_sys::Win32::Networking::WinSock::*;

const OPP: GUID = GUID {
    data1: 0x1105,
    data2: 0,
    data3: 0x1000,
    data4: [0x80, 0, 0, 0x80, 0x5f, 0x9b, 0x34, 0xfb],
};
static WINSOCK: OnceLock<Result<(), i32>> = OnceLock::new();

fn last_error() -> io::Error {
    io::Error::from_raw_os_error(unsafe { WSAGetLastError() })
}

struct Socket(SOCKET);
impl Drop for Socket {
    fn drop(&mut self) {
        unsafe {
            closesocket(self.0);
        }
    }
}

fn new_socket() -> Result<Socket> {
    let started = WINSOCK.get_or_init(|| {
        let mut data = WSADATA::default();
        let result = unsafe { WSAStartup(0x0202, &mut data) };
        if result == 0 {
            Ok(())
        } else {
            Err(result)
        }
    });
    if let Err(code) = started {
        return Err(io::Error::from_raw_os_error(*code))
            .context("Windows Bluetooth sockets unavailable");
    }
    let raw = unsafe { socket(AF_BTH as i32, SOCK_STREAM, BTHPROTO_RFCOMM as i32) };
    if raw == INVALID_SOCKET {
        return Err(last_error()).context("Host Bluetooth adapter unavailable");
    }
    let socket = Socket(raw);
    let required = 1_u32;
    for option in [SO_BTH_AUTHENTICATE, SO_BTH_ENCRYPT] {
        if unsafe {
            setsockopt(
                raw,
                SOL_RFCOMM as i32,
                option as i32,
                (&required as *const u32).cast(),
                size_of::<u32>() as i32,
            )
        } == SOCKET_ERROR
        {
            return Err(last_error()).context("Cannot secure Windows Bluetooth connection");
        }
    }
    let mut nonblocking = 1_u32;
    if unsafe { ioctlsocket(raw, FIONBIO, &mut nonblocking) } == SOCKET_ERROR {
        return Err(last_error()).context("Cannot configure Windows Bluetooth socket");
    }
    Ok(socket)
}

fn wait(raw: SOCKET, write: bool, deadline: Instant) -> io::Result<()> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "Bluetooth socket timed out",
        ));
    }
    let mut set = FD_SET {
        fd_count: 1,
        fd_array: [0; 64],
    };
    set.fd_array[0] = raw;
    let mut errors = set;
    let timeout = TIMEVAL {
        tv_sec: remaining.as_secs().min(i32::MAX as u64) as i32,
        tv_usec: remaining.subsec_micros() as i32,
    };
    let result = unsafe {
        select(
            0,
            if write { ptr::null_mut() } else { &mut set },
            if write { &mut set } else { ptr::null_mut() },
            &mut errors,
            &timeout,
        )
    };
    if result == SOCKET_ERROR {
        return Err(last_error());
    }
    if result == 0 {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "Bluetooth socket timed out",
        ));
    }
    Ok(())
}

pub struct RfcommStream {
    socket: Socket,
    timeout: Duration,
}
impl RfcommStream {
    /// Winsock resolves the Object Push GUID when the channel is omitted.
    pub fn connect(address: &str, channel: Option<u8>, timeout: Duration) -> Result<Self> {
        let peer = parse_address(address)?;
        if let Some(channel) = channel {
            ensure!(
                (1..=30).contains(&channel),
                "Bluetooth Object Push channel must be 1–30"
            );
        }
        let mut encoded = [0; 8];
        encoded[2..].copy_from_slice(&peer);
        let socket = new_socket()?;
        let addr = SOCKADDR_BTH {
            addressFamily: AF_BTH,
            btAddr: u64::from_be_bytes(encoded),
            serviceClassId: if channel.is_some() {
                GUID::default()
            } else {
                OPP
            },
            port: channel.map(u32::from).unwrap_or(0),
        };
        let result = unsafe {
            connect(
                socket.0,
                (&addr as *const SOCKADDR_BTH).cast(),
                size_of::<SOCKADDR_BTH>() as i32,
            )
        };
        if result == SOCKET_ERROR {
            let code = unsafe { WSAGetLastError() };
            if !matches!(code, WSAEWOULDBLOCK | WSAEINPROGRESS | WSAEALREADY) {
                return Err(io::Error::from_raw_os_error(code))
                    .context("Cannot connect to Bluetooth peer; pair it in Windows settings");
            }
            wait(socket.0, true, Instant::now() + timeout)?;
            let mut code = 0_i32;
            let mut length = size_of::<i32>() as i32;
            if unsafe {
                getsockopt(
                    socket.0,
                    SOL_SOCKET,
                    SO_ERROR,
                    (&mut code as *mut i32).cast(),
                    &mut length,
                )
            } == SOCKET_ERROR
            {
                return Err(last_error()).context("Cannot query Bluetooth connection");
            }
            if code != 0 {
                return Err(io::Error::from_raw_os_error(code))
                    .context("Bluetooth connection failed");
            }
        }
        Ok(Self { socket, timeout })
    }
}
impl Read for RfcommStream {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let deadline = Instant::now() + self.timeout;
        loop {
            let count = unsafe {
                recv(
                    self.socket.0,
                    bytes.as_mut_ptr(),
                    bytes.len().min(i32::MAX as usize) as i32,
                    0,
                )
            };
            if count >= 0 {
                return Ok(count as usize);
            }
            let code = unsafe { WSAGetLastError() };
            if code == WSAEWOULDBLOCK {
                wait(self.socket.0, false, deadline)?;
            } else if code != WSAEINTR {
                return Err(io::Error::from_raw_os_error(code));
            }
        }
    }
}
impl Write for RfcommStream {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let deadline = Instant::now() + self.timeout;
        loop {
            let count = unsafe {
                send(
                    self.socket.0,
                    bytes.as_ptr(),
                    bytes.len().min(i32::MAX as usize) as i32,
                    0,
                )
            };
            if count >= 0 {
                return Ok(count as usize);
            }
            let code = unsafe { WSAGetLastError() };
            if code == WSAEWOULDBLOCK {
                wait(self.socket.0, true, deadline)?;
            } else if code != WSAEINTR {
                return Err(io::Error::from_raw_os_error(code));
            }
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub struct RfcommListener {
    _service: Service,
    socket: Socket,
    pub channel: u8,
}
impl RfcommListener {
    pub fn bind(channel: u8) -> Result<Self> {
        ensure!(
            (1..=30).contains(&channel),
            "Bluetooth receiving channel must be 1–30"
        );
        let socket = new_socket()?;
        let mut addr = SOCKADDR_BTH {
            addressFamily: AF_BTH,
            btAddr: 0,
            serviceClassId: GUID::default(),
            port: channel as u32,
        };
        if unsafe {
            bind(
                socket.0,
                (&addr as *const SOCKADDR_BTH).cast(),
                size_of::<SOCKADDR_BTH>() as i32,
            )
        } == SOCKET_ERROR
        {
            return Err(last_error()).context("Cannot bind Windows Bluetooth receiving channel");
        }
        let mut length = size_of::<SOCKADDR_BTH>() as i32;
        if unsafe {
            getsockname(
                socket.0,
                (&mut addr as *mut SOCKADDR_BTH).cast(),
                &mut length,
            )
        } == SOCKET_ERROR
        {
            return Err(last_error()).context("Cannot query Windows Bluetooth receiving address");
        }
        if unsafe { listen(socket.0, 1) } == SOCKET_ERROR {
            return Err(last_error()).context("Cannot listen for Bluetooth files");
        }
        let mut service = Service {
            addr,
            registered: false,
        };
        service.update(RNRSERVICE_REGISTER)?;
        service.registered = true;
        Ok(Self {
            socket,
            _service: service,
            channel,
        })
    }

    pub fn accept(&self, timeout: Duration) -> Result<(RfcommStream, String)> {
        let deadline = Instant::now() + timeout;
        let mut addr = SOCKADDR_BTH::default();
        loop {
            wait(self.socket.0, false, deadline)?;
            let mut length = size_of::<SOCKADDR_BTH>() as i32;
            let raw = unsafe {
                accept(
                    self.socket.0,
                    (&mut addr as *mut SOCKADDR_BTH).cast(),
                    &mut length,
                )
            };
            if raw != INVALID_SOCKET {
                let mut nonblocking = 1_u32;
                let socket = Socket(raw);
                if unsafe { ioctlsocket(raw, FIONBIO, &mut nonblocking) } == SOCKET_ERROR {
                    return Err(last_error().into());
                }
                let peer = addr.btAddr.to_be_bytes();
                return Ok((
                    RfcommStream {
                        socket,
                        timeout: Duration::from_secs(30),
                    },
                    format_address(peer[2..].try_into().unwrap()),
                ));
            }
            let code = unsafe { WSAGetLastError() };
            if !matches!(code, WSAEWOULDBLOCK | WSAEINTR) {
                return Err(io::Error::from_raw_os_error(code).into());
            }
        }
    }
}

struct Service {
    addr: SOCKADDR_BTH,
    registered: bool,
}
impl Service {
    fn update(&self, operation: WSAESETSERVICEOP) -> Result<()> {
        // Winsock creates the SDP record from the standard service GUID and
        // bound RFCOMM address, as documented in Microsoft's Bluetooth sample.
        let mut addr = self.addr;
        let mut csa = CSADDR_INFO {
            LocalAddr: SOCKET_ADDRESS {
                lpSockaddr: (&mut addr as *mut SOCKADDR_BTH).cast(),
                iSockaddrLength: size_of::<SOCKADDR_BTH>() as i32,
            },
            RemoteAddr: SOCKET_ADDRESS {
                lpSockaddr: (&mut addr as *mut SOCKADDR_BTH).cast(),
                iSockaddrLength: size_of::<SOCKADDR_BTH>() as i32,
            },
            iSocketType: SOCK_STREAM,
            iProtocol: BTHPROTO_RFCOMM as i32,
        };
        let mut uuid = OPP;
        let mut name: Vec<u16> = "ph0nix Bluetooth Object Push"
            .encode_utf16()
            .chain([0])
            .collect();
        let query = WSAQUERYSETW {
            dwSize: size_of::<WSAQUERYSETW>() as u32,
            lpServiceClassId: &mut uuid,
            lpszServiceInstanceName: name.as_mut_ptr(),
            dwNameSpace: NS_BTH,
            dwNumberOfCsAddrs: 1,
            lpcsaBuffer: &mut csa,
            ..Default::default()
        };
        if unsafe { WSASetServiceW(&query, operation, 0) } == SOCKET_ERROR {
            return Err(last_error())
                .context("Cannot advertise Windows Bluetooth Object Push service");
        }
        Ok(())
    }
}
impl Drop for Service {
    fn drop(&mut self) {
        if self.registered {
            let _ = self.update(RNRSERVICE_DELETE);
        }
    }
}
