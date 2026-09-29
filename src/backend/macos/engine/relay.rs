use std::ffi::CStr;
use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::job::{Job, connect_worker};
use super::process::path_text;

const NAMES: [&CStr; 3] = [c"General", c"Model", c"Account"];
const MAX_CONNECTIONS: usize = 32;

#[derive(Default)]
pub struct Routes {
    pub general: Option<PathBuf>,
    pub model: Option<PathBuf>,
    pub account: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug)]
pub struct Ports {
    pub general: Option<u16>,
    pub model: Option<u16>,
    pub account: Option<u16>,
}

pub struct Lease {
    _job: Job,
    control: UnixStream,
    pub ports: Ports,
}

impl Lease {
    pub fn start(executable: &Path, directory: &Path, routes: Routes) -> io::Result<Self> {
        let paths = [routes.general, routes.model, routes.account];
        let arguments = paths
            .iter()
            .map(|path| match path {
                Some(path) if path.is_absolute() => path_text(path),
                Some(_) => Err(io::Error::other("broker paths must be absolute")),
                None => Ok(String::new()),
            })
            .collect::<io::Result<Vec<_>>>()?;
        let names: Vec<_> = NAMES
            .iter()
            .zip(&paths)
            .filter_map(|(name, path)| path.as_ref().map(|_| name.to_str().unwrap()))
            .collect();
        let (mut job, mut control) = Job::start(
            executable,
            directory,
            "__macos-relay-worker",
            &arguments,
            &names,
        )?;
        // Only session cleanup may release ports, after every role coalition is empty.
        job.retain_on_drop();
        control.set_nonblocking(false)?;
        control.set_read_timeout(Some(Duration::from_secs(2)))?;
        control.set_write_timeout(Some(Duration::from_secs(2)))?;
        let mut bytes = [0; 6];
        control.read_exact(&mut bytes)?;
        let ports: Vec<_> = bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|bytes| {
                let port = u16::from_be_bytes(*bytes);
                (port != 0).then_some(port)
            })
            .collect();
        if ports
            .iter()
            .zip(&paths)
            .any(|(port, path)| port.is_some() != path.is_some())
        {
            return Err(io::Error::other("invalid activated broker ports"));
        }
        control.write_all(b"S")?;
        Ok(Self {
            _job: job,
            control,
            ports: Ports {
                general: ports[0],
                model: ports[1],
                account: ports[2],
            },
        })
    }

    pub fn revoke(&mut self) -> io::Result<()> {
        self.control.write_all(b"R")?;
        let mut reply = [0];
        self.control.read_exact(&mut reply)?;
        if reply != *b"R" {
            return Err(io::Error::other("invalid revocation response"));
        }
        Ok(())
    }
}

pub fn activate(name: &CStr) -> io::Result<TcpListener> {
    unsafe extern "C" {
        fn launch_activate_socket(
            name: *const libc::c_char,
            descriptors: *mut *mut libc::c_int,
            count: *mut usize,
        ) -> libc::c_int;
    }
    let mut descriptors = std::ptr::null_mut();
    let mut count = 0;
    let error = unsafe { launch_activate_socket(name.as_ptr(), &mut descriptors, &mut count) };
    if error != 0 {
        return Err(io::Error::from_raw_os_error(error));
    }
    let listeners: Vec<_> = (0..count)
        .map(|index| unsafe { TcpListener::from_raw_fd(*descriptors.add(index)) })
        .collect();
    unsafe { libc::free(descriptors.cast()) };
    if listeners.len() != 1 {
        return Err(io::Error::other("expected one activated IPv4 listener"));
    }
    let listener = listeners.into_iter().next().unwrap();
    let address = listener.local_addr()?;
    if !address.is_ipv4() || !address.ip().is_loopback() || address.port() == 0 {
        return Err(io::Error::other("invalid activated listener address"));
    }
    if unsafe { libc::fcntl(listener.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) } == -1 {
        return Err(io::Error::last_os_error());
    }
    listener.set_nonblocking(true)?;
    Ok(listener)
}

pub fn worker(socket: &Path, routes: [Option<PathBuf>; 3]) -> io::Result<()> {
    let mut control = connect_worker(socket)?;
    control.set_read_timeout(Some(Duration::from_secs(2)))?;
    control.set_write_timeout(Some(Duration::from_secs(2)))?;
    let mut listeners = Vec::new();
    let mut ports = Vec::new();
    for (name, path) in NAMES.iter().zip(routes) {
        if let Some(path) = path {
            let listener = activate(name)?;
            ports.extend_from_slice(&listener.local_addr()?.port().to_be_bytes());
            listeners.push((listener, path));
        } else {
            ports.extend_from_slice(&0_u16.to_be_bytes());
        }
    }
    control.write_all(&ports)?;
    let mut command = [0];
    control.read_exact(&mut command)?;
    if command != *b"S" {
        return Err(io::Error::other("invalid broker authorization"));
    }
    control.set_nonblocking(true)?;
    let mut connections = Vec::<Connection>::new();
    let mut revoked = false;
    loop {
        match control.read(&mut command) {
            Ok(0) => return Ok(()),
            Ok(_) if command == *b"R" => {
                connections.clear();
                revoked = true;
                control.write_all(b"R")?;
            }
            Ok(_) => return Err(io::Error::other("invalid broker command")),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
        // A bounded batch keeps shutdown/revocation responsive under incoming load.
        for (listener, path) in &listeners {
            for _ in 0..MAX_CONNECTIONS {
                match listener.accept() {
                    Ok((tcp, _)) => {
                        if !revoked
                            && connections.len() < MAX_CONNECTIONS
                            && let Ok(connection) = Connection::new(tcp, path)
                        {
                            connections.push(connection);
                        }
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(error) => return Err(error),
                }
            }
        }
        connections.retain_mut(|connection| connection.step().unwrap_or(false));
        std::thread::sleep(Duration::from_millis(2));
    }
}

struct Connection {
    tcp: TcpStream,
    unix: UnixStream,
    upstream: Direction,
    downstream: Direction,
}

impl Connection {
    fn new(tcp: TcpStream, path: &Path) -> io::Result<Self> {
        tcp.set_nonblocking(true)?;
        let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
        let bytes = path.as_os_str().as_bytes();
        if bytes.len() >= address.sun_path.len() || bytes.contains(&0) {
            return Err(io::Error::other("invalid broker socket path"));
        }
        address.sun_len = std::mem::size_of_val(&address) as u8;
        address.sun_family = libc::AF_UNIX as _;
        for (target, byte) in address.sun_path.iter_mut().zip(bytes) {
            *target = *byte as _;
        }
        let fd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
        if fd == -1 {
            return Err(io::Error::last_os_error());
        }
        let unix = unsafe { UnixStream::from_raw_fd(fd) };
        unix.set_nonblocking(true)?;
        if unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } == -1 {
            return Err(io::Error::last_os_error());
        }
        if unsafe {
            libc::connect(
                fd,
                (&address as *const libc::sockaddr_un).cast(),
                address.sun_len as _,
            )
        } != 0
        {
            // Saturated or unavailable brokers are not retried or replayed.
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            tcp,
            unix,
            upstream: Direction::default(),
            downstream: Direction::default(),
        })
    }

    fn step(&mut self) -> io::Result<bool> {
        self.upstream.step(&mut self.tcp, &mut self.unix)?;
        self.downstream.step(&mut self.unix, &mut self.tcp)?;
        Ok(!(self.upstream.closed && self.downstream.closed))
    }
}

#[derive(Default)]
struct Direction {
    bytes: Vec<u8>,
    offset: usize,
    eof: bool,
    closed: bool,
}

impl Direction {
    fn step(
        &mut self,
        input: &mut impl Read,
        output: &mut (impl Write + AsRawFd),
    ) -> io::Result<()> {
        if self.closed {
            return Ok(());
        }
        if self.offset == self.bytes.len() && !self.eof {
            self.bytes.resize(8192, 0);
            self.offset = 0;
            match input.read(&mut self.bytes) {
                Ok(count) => {
                    self.bytes.truncate(count);
                    self.eof = count == 0;
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) =>
                {
                    self.bytes.clear()
                }
                Err(error) => return Err(error),
            }
        }
        if self.offset < self.bytes.len() {
            match output.write(&self.bytes[self.offset..]) {
                Ok(0) => return Err(io::Error::other("broker connection closed")),
                Ok(count) => self.offset += count,
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) => {}
                Err(error) => return Err(error),
            }
        }
        if self.eof && self.offset == self.bytes.len() {
            if unsafe { libc::shutdown(output.as_raw_fd(), libc::SHUT_WR) } != 0 {
                let error = io::Error::last_os_error();
                // Darwin can report a closed peer while response bytes are still unread.
                if error.kind() != io::ErrorKind::NotConnected {
                    return Err(error);
                }
            }
            self.closed = true;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn half_close_after_broker_exit_preserves_buffered_response() {
        let (mut connection, mut broker) = UnixStream::pair().unwrap();
        broker.write_all(b"response").unwrap();
        drop(broker);

        let mut direction = Direction::default();
        direction.step(&mut io::empty(), &mut connection).unwrap();
        assert!(direction.closed);
        let mut response = Vec::new();
        connection.read_to_end(&mut response).unwrap();
        assert_eq!(response, b"response");
    }
}
