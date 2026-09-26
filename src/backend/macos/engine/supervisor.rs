use std::ffi::CString;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use super::coalition::Coalition;
use super::job::{Job, connect_worker};
use super::session::Role;

pub const MAX_REQUEST: usize = 16 * 1024;
pub const MAX_OUTPUT: usize = 32 * 1024;
const DEADLINE: Duration = Duration::from_secs(2);

unsafe extern "C" {
    fn posix_spawn_file_actions_addchdir_np(
        actions: *mut libc::posix_spawn_file_actions_t,
        directory: *const libc::c_char,
    ) -> libc::c_int;
}

pub struct Supervisor {
    socket: PathBuf,
    stopped: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl Supervisor {
    pub fn start(
        executable: PathBuf,
        socket: PathBuf,
        profile: String,
        workspace: PathBuf,
        home: PathBuf,
    ) -> io::Result<Self> {
        let control = socket
            .parent()
            .ok_or_else(|| io::Error::other("invalid tool socket"))?
            .to_owned();
        Self::start_in(executable, socket, control, profile, workspace, home)
    }

    pub fn start_in(
        executable: PathBuf,
        socket: PathBuf,
        control: PathBuf,
        profile: String,
        workspace: PathBuf,
        home: PathBuf,
    ) -> io::Result<Self> {
        Self::start_configured(
            executable,
            socket,
            control,
            Role {
                profile,
                workspace,
                home,
            },
            None,
            DEADLINE,
            Vec::new(),
        )
    }

    pub fn start_configured(
        executable: PathBuf,
        socket: PathBuf,
        control: PathBuf,
        role: Role,
        environment: Option<PathBuf>,
        timeout: Duration,
        command_prefix: Vec<String>,
    ) -> io::Result<Self> {
        let listener = UnixListener::bind(&socket)?;
        fs::set_permissions(&socket, fs::Permissions::from_mode(0o600))?;
        listener.set_nonblocking(true)?;
        let stopped = Arc::new(AtomicBool::new(false));
        let stop = Arc::clone(&stopped);
        let worker = thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let result = (|| {
                            stream.set_nonblocking(true)?;
                            let body = receive(&mut stream, MAX_REQUEST, &stop, None)?;
                            let body = prefixed_request(&body, &command_prefix)?;
                            let mut configuration = vec![
                                role.profile.clone(),
                                path_text(&role.workspace)?,
                                path_text(&role.home)?,
                                timeout.as_secs().to_string(),
                            ];
                            if let Some(environment) = &environment {
                                configuration
                                    .extend(["--environment".into(), path_text(environment)?]);
                            }
                            let (mut job, mut worker) = Job::start(
                                &executable,
                                &control,
                                "__macos-exec-worker",
                                &configuration,
                                &[],
                            )?;
                            send(&mut worker, &body, &stop)?;
                            let response = receive_until(
                                &mut worker,
                                MAX_OUTPUT + 32,
                                &stop,
                                Some(&mut stream),
                                timeout + DEADLINE,
                            )?;
                            drop(worker);
                            job.finish()?;
                            send(&mut stream, &response, &stop)
                        })();
                        if result.is_err() {
                            // Malformed or disconnected requests never get replayed.
                            let _ = send(&mut stream, b"126\nrequest failed", &stop);
                        }
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5))
                    }
                    Err(_) => break,
                }
            }
        });
        Ok(Self {
            socket,
            stopped,
            worker: Some(worker),
        })
    }
}

impl Drop for Supervisor {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take()
            && worker.join().is_err()
        {
            eprintln!("native tool supervisor panicked; session recovery required");
        }
        let _ = fs::remove_file(&self.socket);
    }
}

fn prefixed_request(body: &[u8], prefix: &[String]) -> io::Result<Vec<u8>> {
    arguments(body)?;
    if prefix.is_empty() {
        return Ok(body.to_vec());
    }
    if prefix.iter().any(|argument| argument.contains('\0')) {
        return Err(io::Error::other("invalid command prefix"));
    }
    let mut request = format!("{}\0", prefix.join("\0")).into_bytes();
    request.extend_from_slice(body);
    arguments(&request)?;
    if request.len() > MAX_REQUEST {
        return Err(io::Error::other("activated tool request too large"));
    }
    Ok(request)
}

pub fn request(socket: &Path, arguments: &[String]) -> io::Result<(i32, Vec<u8>)> {
    if arguments.iter().any(|argument| argument.contains('\0')) {
        return Err(io::Error::other("arguments cannot contain NUL bytes"));
    }
    let mut stream = UnixStream::connect(socket)?;
    stream.set_nonblocking(true)?;
    let body = format!("{}\0", arguments.join("\0"));
    let stop = AtomicBool::new(false);
    send(&mut stream, body.as_bytes(), &stop)?;
    let response = receive(&mut stream, MAX_OUTPUT + 32, &stop, None)?;
    let separator = response
        .iter()
        .position(|byte| *byte == b'\n')
        .ok_or_else(|| io::Error::other("invalid response"))?;
    let status = std::str::from_utf8(&response[..separator])
        .ok()
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| io::Error::other("invalid response status"))?;
    Ok((status, response[separator + 1..].to_vec()))
}

fn send(stream: &mut UnixStream, body: &[u8], stop: &AtomicBool) -> io::Result<()> {
    let mut frame = (body.len() as u32).to_be_bytes().to_vec();
    frame.extend_from_slice(body);
    let deadline = Instant::now() + DEADLINE;
    let mut remaining = frame.as_slice();
    while !remaining.is_empty() {
        bounded(deadline, stop)?;
        match stream.write(remaining) {
            Ok(0) => return Err(io::Error::other("connection closed")),
            Ok(count) => remaining = &remaining[count..],
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(5))
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn receive(
    stream: &mut UnixStream,
    limit: usize,
    stop: &AtomicBool,
    peer: Option<&mut UnixStream>,
) -> io::Result<Vec<u8>> {
    receive_until(stream, limit, stop, peer, DEADLINE)
}

fn receive_until(
    stream: &mut UnixStream,
    limit: usize,
    stop: &AtomicBool,
    mut peer: Option<&mut UnixStream>,
    timeout: Duration,
) -> io::Result<Vec<u8>> {
    let deadline = Instant::now() + timeout;
    let mut frame = vec![0; 4];
    let mut position = 0;
    loop {
        bounded(deadline, stop)?;
        if let Some(peer) = peer.as_deref_mut() {
            connected(peer)?;
        }
        match stream.read(&mut frame[position..]) {
            Ok(0) => return Err(io::Error::other("connection closed")),
            Ok(count) => position += count,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(5))
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
        if position == frame.len() {
            if position == 4 {
                let size = u32::from_be_bytes(frame[..4].try_into().unwrap()) as usize;
                if size == 0 || size > limit {
                    return Err(io::Error::other("invalid frame length"));
                }
                frame.resize(4 + size, 0);
            } else {
                return Ok(frame[4..].to_vec());
            }
        }
    }
}

fn bounded(deadline: Instant, stop: &AtomicBool) -> io::Result<()> {
    if stop.load(Ordering::Relaxed) || Instant::now() >= deadline {
        Err(io::Error::new(io::ErrorKind::TimedOut, "request stopped"))
    } else {
        Ok(())
    }
}

pub fn worker(
    socket: &Path,
    profile: &str,
    workspace: &Path,
    home: &Path,
    deadline: Duration,
) -> io::Result<()> {
    worker_configured(socket, profile, workspace, home, deadline, None)
}

pub fn worker_stream(
    stream: &mut UnixStream,
    profile: &str,
    workspace: &Path,
    home: &Path,
    deadline: Duration,
) -> io::Result<()> {
    serve_worker(stream, profile, workspace, home, deadline, &[])
}

pub fn worker_configured(
    socket: &Path,
    profile: &str,
    workspace: &Path,
    home: &Path,
    deadline: Duration,
    environment: Option<&Path>,
) -> io::Result<()> {
    let mut stream = connect_worker(socket)?;
    let environment = read_environment(environment)?;
    serve_worker(
        &mut stream,
        profile,
        workspace,
        home,
        deadline,
        &environment,
    )
}

fn serve_worker(
    stream: &mut UnixStream,
    profile: &str,
    workspace: &Path,
    home: &Path,
    deadline: Duration,
    environment: &[String],
) -> io::Result<()> {
    let coalition = Coalition::read(std::process::id() as i32)?;
    let result = serve(
        stream,
        profile,
        workspace,
        home,
        coalition,
        deadline,
        environment,
    );
    coalition.terminate(Some(std::process::id() as i32))?;
    result
}

pub(super) fn connected(stream: &mut UnixStream) -> io::Result<()> {
    match stream.read(&mut [0]) {
        Ok(_) => Err(io::Error::other(
            "request disconnected or has trailing data",
        )),
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(()),
        Err(error) => Err(error),
    }
}

pub(super) fn arguments(bytes: &[u8]) -> io::Result<Vec<&str>> {
    let body = std::str::from_utf8(bytes).map_err(io::Error::other)?;
    let body = body
        .strip_suffix('\0')
        .ok_or_else(|| io::Error::other("unterminated request"))?;
    let arguments: Vec<_> = body.split('\0').collect();
    if arguments.len() > 32 || !arguments[0].starts_with('/') {
        return Err(io::Error::other("invalid arguments"));
    }
    Ok(arguments)
}

fn serve(
    stream: &mut UnixStream,
    profile: &str,
    workspace: &Path,
    home: &Path,
    coalition: Coalition,
    execution_deadline: Duration,
    environment: &[String],
) -> io::Result<()> {
    let stop = &AtomicBool::new(false);
    stream.set_nonblocking(true)?;
    let bytes = receive(stream, MAX_REQUEST, stop, None)?;
    let arguments = arguments(&bytes)?;
    let input = File::open("/dev/null")?;
    let (mut output, writer) = std::io::pipe()?;
    if unsafe { libc::fcntl(output.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) } == -1 {
        return Err(io::Error::last_os_error());
    }
    let mut child = spawn(
        profile,
        workspace,
        home,
        &arguments,
        coalition,
        environment,
        [input.as_raw_fd(), writer.as_raw_fd(), writer.as_raw_fd()],
    )?;
    drop(writer);
    let deadline = Instant::now() + execution_deadline;
    let mut bytes = Vec::new();
    let status = loop {
        bounded(deadline, stop)?;
        connected(stream)?;
        drain(&mut output, &mut bytes)?;
        if child.exited()? {
            break child.finish()?;
        }
        thread::sleep(Duration::from_millis(5));
    };
    drain(&mut output, &mut bytes)?;
    let mut response = format!("{status}\n").into_bytes();
    response.extend_from_slice(&bytes);
    send(stream, &response, stop)
}

fn drain(output: &mut std::io::PipeReader, bytes: &mut Vec<u8>) -> io::Result<()> {
    let mut buffer = [0; 4096];
    loop {
        match output.read(&mut buffer) {
            Ok(0) => return Ok(()),
            Ok(count) => {
                if bytes.len() + count > MAX_OUTPUT {
                    return Err(io::Error::other("tool output limit exceeded"));
                }
                bytes.extend_from_slice(&buffer[..count]);
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(()),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
}

pub(super) struct Child(pub(super) libc::pid_t, Coalition);

impl Child {
    pub(super) fn exited(&self) -> io::Result<bool> {
        let mut info = unsafe { std::mem::zeroed() };
        if unsafe {
            libc::waitid(
                libc::P_PID,
                self.0 as _,
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        } == -1
        {
            return Err(io::Error::last_os_error());
        }
        Ok(unsafe { info.si_pid() } != 0)
    }

    pub(super) fn finish(&mut self) -> io::Result<i32> {
        self.1.terminate(Some(std::process::id() as i32))?;
        let mut status = 0;
        loop {
            if unsafe { libc::waitpid(self.0, &mut status, 0) } != -1 {
                break;
            }
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
        self.0 = 0;
        Ok(if libc::WIFEXITED(status) {
            libc::WEXITSTATUS(status)
        } else {
            128 + libc::WTERMSIG(status)
        })
    }
}

impl Drop for Child {
    fn drop(&mut self) {
        if self.0 != 0 {
            let _ = self.finish();
        }
    }
}

pub(super) fn spawn(
    profile: &str,
    workspace: &Path,
    home: &Path,
    arguments: &[&str],
    coalition: Coalition,
    extra_environment: &[String],
    stdio: [libc::c_int; 3],
) -> io::Result<Child> {
    let executable = cstring("/usr/bin/sandbox-exec")?;
    let mut argv = vec![
        executable.clone(),
        cstring("-p")?,
        cstring(profile)?,
        cstring("--")?,
    ];
    for argument in arguments {
        argv.push(cstring(argument)?);
    }
    let mut argv: Vec<_> = argv.iter().map(|value| value.as_ptr() as *mut _).collect();
    argv.push(std::ptr::null_mut());
    let mut environment = vec![
        cstring(&format!("HOME={}", home.display()))?,
        cstring(&format!("TMPDIR={}", home.display()))?,
        cstring("PATH=/usr/bin:/bin")?,
        cstring("LC_ALL=C")?,
    ];
    for entry in extra_environment {
        let key = entry
            .split_once('=')
            .ok_or_else(|| io::Error::other("invalid environment entry"))?
            .0;
        let prefix = format!("{key}=");
        environment.retain(|value| !value.as_bytes().starts_with(prefix.as_bytes()));
        environment.push(cstring(entry)?);
    }
    let mut environment: Vec<_> = environment
        .iter()
        .map(|value| value.as_ptr() as *mut _)
        .collect();
    environment.push(std::ptr::null_mut());
    let workspace = CString::new(workspace.as_os_str().as_bytes()).map_err(io::Error::other)?;
    let mut actions = unsafe { std::mem::zeroed() };
    let mut attributes = unsafe { std::mem::zeroed() };
    check(unsafe { libc::posix_spawn_file_actions_init(&mut actions) })?;
    let result = (|| {
        check(unsafe { libc::posix_spawnattr_init(&mut attributes) })?;
        let result = (|| {
            check(unsafe {
                posix_spawn_file_actions_addchdir_np(&mut actions, workspace.as_ptr())
            })?;
            for (target, source) in stdio.into_iter().enumerate() {
                check(unsafe {
                    libc::posix_spawn_file_actions_adddup2(&mut actions, source, target as _)
                })?;
            }
            let mut signals = unsafe { std::mem::zeroed() };
            unsafe {
                libc::sigemptyset(&mut signals);
            }
            check(unsafe { libc::posix_spawnattr_setsigmask(&mut attributes, &signals) })?;
            unsafe {
                libc::sigfillset(&mut signals);
            }
            check(unsafe { libc::posix_spawnattr_setsigdefault(&mut attributes, &signals) })?;
            check(unsafe { libc::posix_spawnattr_setpgroup(&mut attributes, 0) })?;
            check(unsafe {
                libc::posix_spawnattr_setflags(
                    &mut attributes,
                    (libc::POSIX_SPAWN_CLOEXEC_DEFAULT
                        | libc::POSIX_SPAWN_SETPGROUP
                        | libc::POSIX_SPAWN_SETSIGMASK
                        | libc::POSIX_SPAWN_SETSIGDEF) as _,
                )
            })?;
            let mut pid = 0;
            check(unsafe {
                libc::posix_spawn(
                    &mut pid,
                    executable.as_ptr(),
                    &actions,
                    &attributes,
                    argv.as_ptr(),
                    environment.as_ptr(),
                )
            })?;
            Ok(Child(pid, coalition))
        })();
        unsafe {
            libc::posix_spawnattr_destroy(&mut attributes);
        }
        result
    })();
    unsafe {
        libc::posix_spawn_file_actions_destroy(&mut actions);
    }
    result
}

pub(super) fn read_environment(path: Option<&Path>) -> io::Result<Vec<String>> {
    let Some(path) = path else {
        return Ok(Vec::new());
    };
    let mut bytes = String::new();
    File::open(path)?
        .take(16 * 1024 + 1)
        .read_to_string(&mut bytes)?;
    if bytes.len() > 16 * 1024 {
        return Err(io::Error::other("environment too large"));
    }
    bytes
        .split_terminator('\0')
        .map(|entry| {
            if !entry.contains('=') {
                return Err(io::Error::other("invalid environment entry"));
            }
            Ok(entry.to_owned())
        })
        .collect()
}

fn check(error: libc::c_int) -> io::Result<()> {
    if error == 0 {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(error))
    }
}

pub(super) fn path_text(path: &Path) -> io::Result<String> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| io::Error::other("non-UTF-8 native path"))
}

fn cstring(value: &str) -> io::Result<CString> {
    CString::new(value).map_err(io::Error::other)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activation_preserves_arguments_without_evaluating_them() {
        let body = b"/bin/bash\0-c\0echo '$data'; exit 7\0";
        let prefix = [
            "/selected/bash",
            "--noprofile",
            "--norc",
            "/private/activation script",
        ]
        .map(str::to_owned);
        let request = prefixed_request(body, &prefix).unwrap();
        assert_eq!(
            arguments(&request).unwrap(),
            [
                "/selected/bash",
                "--noprofile",
                "--norc",
                "/private/activation script",
                "/bin/bash",
                "-c",
                "echo '$data'; exit 7"
            ]
        );
        assert_eq!(prefixed_request(body, &[]).unwrap(), body);
        assert!(prefixed_request(b"relative\0", &prefix).is_err());
        assert!(prefixed_request(body, &["/bad\0argument".into()]).is_err());
        assert!(prefixed_request(body, &["relative".into()]).is_err());
        let large = format!("/bin/bash\0{}\0", "x".repeat(MAX_REQUEST));
        assert!(prefixed_request(large.as_bytes(), &prefix).is_err());
    }
}
