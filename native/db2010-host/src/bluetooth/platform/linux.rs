// SPDX-License-Identifier: GPL-2.0-or-later
use super::{format_address, opp_record, parse_address};
use anyhow::{bail, ensure, Context, Result};
use libloading::Library;
use std::ffi::c_void;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::ptr;
use std::time::{Duration, Instant};

const AF_BLUETOOTH: i32 = 31;
const RFCOMM: i32 = 3;
#[repr(C)]
struct RcAddress {
    family: u16,
    address: [u8; 6],
    channel: u8,
}

fn wait(socket: i32, events: i16, deadline: Instant) -> io::Result<()> {
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Bluetooth socket timed out",
            ));
        }
        let mut pfd = libc::pollfd {
            fd: socket,
            events,
            revents: 0,
        };
        let result = unsafe {
            libc::poll(
                &mut pfd,
                1,
                remaining.as_millis().clamp(1, i32::MAX as u128) as i32,
            )
        };
        if result > 0 {
            return Ok(());
        }
        if result == 0 {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Bluetooth socket timed out",
            ));
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

fn new_socket() -> Result<OwnedFd> {
    let fd = unsafe {
        libc::socket(
            AF_BLUETOOTH,
            libc::SOCK_STREAM | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
            RFCOMM,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error()).context("Host Bluetooth adapter unavailable");
    }
    let socket = unsafe { OwnedFd::from_raw_fd(fd) };
    // Request authentication and encryption from the host Bluetooth stack.
    let security = [2_u8, 0]; // struct bt_security: BT_SECURITY_MEDIUM
    let result =
        unsafe { libc::setsockopt(fd, 274, 4, security.as_ptr().cast(), security.len() as u32) };
    if result < 0 {
        return Err(io::Error::last_os_error()).context("Cannot secure host Bluetooth connection");
    }
    Ok(socket)
}

pub struct RfcommStream {
    socket: OwnedFd,
    timeout: Duration,
}
impl RfcommStream {
    /// If channel is omitted, BlueZ SDP discovers the peer's Object Push port.
    pub fn connect(address: &str, channel: Option<u8>, timeout: Duration) -> Result<Self> {
        let mut peer = parse_address(address)?;
        peer.reverse(); // BlueZ bdaddr_t is little endian.
        let channel = match channel {
            Some(channel) => channel,
            None => Bluez::load()?.discover(peer)?,
        };
        ensure!(
            (1..=30).contains(&channel),
            "Bluetooth Object Push channel must be 1–30"
        );
        let socket = new_socket()?;
        let addr = RcAddress {
            family: AF_BLUETOOTH as u16,
            address: peer,
            channel,
        };
        let result = unsafe {
            libc::connect(
                socket.as_raw_fd(),
                (&addr as *const RcAddress).cast(),
                std::mem::size_of::<RcAddress>() as u32,
            )
        };
        if result < 0 {
            let error = io::Error::last_os_error();
            if !matches!(
                error.raw_os_error(),
                Some(libc::EINPROGRESS) | Some(libc::EAGAIN)
            ) {
                return Err(error)
                    .context("Cannot connect to Bluetooth peer; pair it in host settings");
            }
            wait(socket.as_raw_fd(), libc::POLLOUT, Instant::now() + timeout)?;
            let mut code = 0_i32;
            let mut length = std::mem::size_of::<i32>() as u32;
            let result = unsafe {
                libc::getsockopt(
                    socket.as_raw_fd(),
                    libc::SOL_SOCKET,
                    libc::SO_ERROR,
                    (&mut code as *mut i32).cast(),
                    &mut length,
                )
            };
            if result < 0 {
                return Err(io::Error::last_os_error())
                    .context("Cannot query Bluetooth connection");
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
                libc::recv(
                    self.socket.as_raw_fd(),
                    bytes.as_mut_ptr().cast(),
                    bytes.len(),
                    0,
                )
            };
            if count >= 0 {
                return Ok(count as usize);
            }
            let error = io::Error::last_os_error();
            match error.kind() {
                io::ErrorKind::WouldBlock => wait(self.socket.as_raw_fd(), libc::POLLIN, deadline)?,
                io::ErrorKind::Interrupted => {}
                _ => return Err(error),
            }
        }
    }
}
impl Write for RfcommStream {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let deadline = Instant::now() + self.timeout;
        loop {
            let count = unsafe {
                libc::send(
                    self.socket.as_raw_fd(),
                    bytes.as_ptr().cast(),
                    bytes.len(),
                    libc::MSG_NOSIGNAL,
                )
            };
            if count >= 0 {
                return Ok(count as usize);
            }
            let error = io::Error::last_os_error();
            match error.kind() {
                io::ErrorKind::WouldBlock => {
                    wait(self.socket.as_raw_fd(), libc::POLLOUT, deadline)?
                }
                io::ErrorKind::Interrupted => {}
                _ => return Err(error),
            }
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub struct RfcommListener {
    _service: Registration,
    socket: OwnedFd,
    pub channel: u8,
}
impl RfcommListener {
    /// Registers a temporary standard OPP SDP service. Requires a host adapter
    /// and BlueZ's local SDP compatibility socket; registration ends on drop.
    pub fn bind(channel: u8) -> Result<Self> {
        ensure!(
            (1..=30).contains(&channel),
            "Bluetooth receiving channel must be 1–30"
        );
        let socket = new_socket()?;
        let addr = RcAddress {
            family: AF_BLUETOOTH as u16,
            address: [0; 6],
            channel,
        };
        let result = unsafe {
            libc::bind(
                socket.as_raw_fd(),
                (&addr as *const RcAddress).cast(),
                std::mem::size_of::<RcAddress>() as u32,
            )
        };
        if result < 0 {
            return Err(io::Error::last_os_error())
                .context("Cannot bind host Bluetooth receiving channel");
        }
        if unsafe { libc::listen(socket.as_raw_fd(), 1) } < 0 {
            return Err(io::Error::last_os_error()).context("Cannot listen for Bluetooth files");
        }
        let service = Bluez::load()?.register(channel)?;
        Ok(Self {
            socket,
            _service: service,
            channel,
        })
    }

    /// Call with a short timeout to support UI cancellation without blocking.
    pub fn accept(&self, timeout: Duration) -> Result<(RfcommStream, String)> {
        let deadline = Instant::now() + timeout;
        let mut addr = RcAddress {
            family: 0,
            address: [0; 6],
            channel: 0,
        };
        loop {
            wait(self.socket.as_raw_fd(), libc::POLLIN, deadline)?;
            let mut length = std::mem::size_of::<RcAddress>() as u32;
            let fd = unsafe {
                libc::accept4(
                    self.socket.as_raw_fd(),
                    (&mut addr as *mut RcAddress).cast(),
                    &mut length,
                    libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
                )
            };
            if fd >= 0 {
                let socket = unsafe { OwnedFd::from_raw_fd(fd) };
                addr.address.reverse();
                return Ok((
                    RfcommStream {
                        socket,
                        timeout: Duration::from_secs(30),
                    },
                    format_address(addr.address),
                ));
            }
            let error = io::Error::last_os_error();
            if !matches!(
                error.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
            ) {
                return Err(error.into());
            }
        }
    }
}

#[repr(C)]
struct SdpList {
    next: *mut SdpList,
    data: *mut c_void,
}
#[repr(C)]
struct SdpUuid {
    kind: u8,
    value: [u32; 4],
}
type Session = c_void;
type Record = c_void;
type ListFree = unsafe extern "C" fn(*mut SdpList, Option<unsafe extern "C" fn(*mut c_void)>);
type RecordFree = unsafe extern "C" fn(*mut Record);

struct Bluez {
    _library: Library,
    connect: unsafe extern "C" fn(*const [u8; 6], *const [u8; 6], u32) -> *mut Session,
    close: unsafe extern "C" fn(*mut Session) -> i32,
    append: unsafe extern "C" fn(*mut SdpList, *mut c_void) -> *mut SdpList,
    list_free: ListFree,
    uuid: unsafe extern "C" fn(*mut SdpUuid, u16) -> *mut SdpUuid,
    search: unsafe extern "C" fn(
        *mut Session,
        *const SdpList,
        i32,
        *const SdpList,
        *mut *mut SdpList,
    ) -> i32,
    protocols: unsafe extern "C" fn(*const Record, *mut *mut SdpList) -> i32,
    port: unsafe extern "C" fn(*const SdpList, i32) -> i32,
    record_free: RecordFree,
    extract: unsafe extern "C" fn(*const u8, i32, *mut i32) -> *mut Record,
    register: unsafe extern "C" fn(*mut Session, *mut Record, u8) -> i32,
    unregister: unsafe extern "C" fn(*mut Session, *mut Record) -> i32,
}

impl Bluez {
    fn load() -> Result<Self> {
        // All ABI declarations above match BlueZ's public sdp.h/sdp_lib.h.
        let library = unsafe { Library::new("libbluetooth.so.3") }.context(
            "Install the BlueZ libbluetooth runtime for host Bluetooth discovery and receive",
        )?;
        unsafe {
            Ok(Self {
                connect: *library.get(b"sdp_connect\0")?,
                close: *library.get(b"sdp_close\0")?,
                append: *library.get(b"sdp_list_append\0")?,
                list_free: *library.get(b"sdp_list_free\0")?,
                uuid: *library.get(b"sdp_uuid16_create\0")?,
                search: *library.get(b"sdp_service_search_attr_req\0")?,
                protocols: *library.get(b"sdp_get_access_protos\0")?,
                port: *library.get(b"sdp_get_proto_port\0")?,
                record_free: *library.get(b"sdp_record_free\0")?,
                extract: *library.get(b"sdp_extract_pdu\0")?,
                register: *library.get(b"sdp_record_register\0")?,
                unregister: *library.get(b"sdp_record_unregister\0")?,
                _library: library,
            })
        }
    }

    unsafe fn free_protocols(&self, mut list: *mut SdpList) {
        let root = list;
        while !list.is_null() {
            (self.list_free)((*list).data.cast(), None);
            list = (*list).next;
        }
        (self.list_free)(root, None);
    }

    fn discover(&self, peer: [u8; 6]) -> Result<u8> {
        unsafe {
            let session = (self.connect)(&[0; 6], &peer, 0);
            if session.is_null() {
                return Err(io::Error::last_os_error())
                    .context("Bluetooth SDP discovery failed; pair the peer first");
            }
            let mut uuid = SdpUuid {
                kind: 0,
                value: [0; 4],
            };
            (self.uuid)(&mut uuid, 0x1105);
            let search = (self.append)(ptr::null_mut(), (&mut uuid as *mut SdpUuid).cast());
            let mut range = 0x0000_ffff_u32;
            let attrs = (self.append)(ptr::null_mut(), (&mut range as *mut u32).cast());
            let mut response: *mut SdpList = ptr::null_mut();
            let status = (self.search)(session, search, 2, attrs, &mut response);
            let error = io::Error::last_os_error();
            (self.list_free)(search, None);
            (self.list_free)(attrs, None);
            (self.close)(session);
            let mut found = None;
            let mut node = response;
            while !node.is_null() {
                let mut protos = ptr::null_mut();
                if (self.protocols)((*node).data, &mut protos) == 0 {
                    let channel = (self.port)(protos, RFCOMM);
                    if (1..=30).contains(&channel) && found.is_none() {
                        found = Some(channel as u8);
                    }
                    self.free_protocols(protos);
                }
                node = (*node).next;
            }
            (self.list_free)(response, Some(self.record_free));
            if status < 0 {
                return Err(error).context("Bluetooth Object Push service discovery failed");
            }
            found.context("The paired Bluetooth peer does not advertise Object Push")
        }
    }

    fn register(self, channel: u8) -> Result<Registration> {
        unsafe {
            // BDADDR_LOCAL is the local SDP daemon, not a radio address.
            let session = (self.connect)(&[0; 6], &[0, 0, 0, 0xff, 0xff, 0xff], 0);
            if session.is_null() {
                return Err(io::Error::last_os_error()).context(
                    "BlueZ local SDP service unavailable; receiving needs bluetoothd's SDP compatibility socket");
            }
            let encoded = opp_record(channel);
            let mut scanned = 0;
            let record = (self.extract)(encoded.as_ptr(), encoded.len() as i32, &mut scanned);
            if record.is_null() || scanned as usize != encoded.len() {
                if !record.is_null() {
                    (self.record_free)(record);
                }
                (self.close)(session);
                bail!("Cannot create Bluetooth Object Push SDP record");
            }
            if (self.register)(session, record, 0) < 0 {
                let error = io::Error::last_os_error();
                (self.record_free)(record);
                (self.close)(session);
                return Err(error).context("Cannot advertise host Bluetooth Object Push service");
            }
            Ok(Registration {
                library: self,
                session,
                record,
            })
        }
    }
}

struct Registration {
    library: Bluez,
    session: *mut Session,
    record: *mut Record,
}
// The registration is owned by one listener. Its pointers are only used by
// that owner's destructor; BlueZ and dlopen support moving ownership to a worker.
unsafe impl Send for Registration {}
impl Drop for Registration {
    fn drop(&mut self) {
        unsafe {
            (self.library.unregister)(self.session, self.record);
            (self.library.record_free)(self.record);
            (self.library.close)(self.session);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sdp_record_parses_with_the_real_bluez_library_and_correct_channel() {
        // Optional runtime dependency: portable builds need no BlueZ adapter.
        let Ok(bluez) = Bluez::load() else {
            return;
        };
        for channel in [1, 12, 30] {
            let encoded = opp_record(channel);
            let mut scanned = 0;
            unsafe {
                let record = (bluez.extract)(encoded.as_ptr(), encoded.len() as i32, &mut scanned);
                assert!(!record.is_null());
                assert_eq!(scanned as usize, encoded.len());
                let mut protos = ptr::null_mut();
                assert_eq!((bluez.protocols)(record, &mut protos), 0);
                assert_eq!((bluez.port)(protos, RFCOMM), channel as i32);
                bluez.free_protocols(protos);
                (bluez.record_free)(record);
            }
        }
    }
}
