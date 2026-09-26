#![cfg(target_os = "macos")]

#[path = "native/session.rs"]
mod native_session;

use std::fs::{self, DirBuilder, File};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, symlink};
use std::os::unix::net::UnixListener;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

const F_SETFD: i32 = 2;
const FD_CLOEXEC: i32 = 1;
const SIGKILL: i32 = 9;
static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

unsafe extern "C" {
    fn dup2(source: i32, destination: i32) -> i32;
    fn fcntl(descriptor: i32, command: i32, ...) -> i32;
    fn kill(pid: i32, signal: i32) -> i32;
}

struct Fixture {
    root: PathBuf,
    executable: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let id = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
        let root = PathBuf::from(format!(
            "/var/tmp/slopbox-seatbelt-{}-{id}",
            std::process::id()
        ));
        DirBuilder::new().mode(0o700).create(&root).unwrap();
        let root = root.canonicalize().unwrap();
        for name in ["workspace", "harness", "tool", "outside", "control"] {
            fs::create_dir(root.join(name)).unwrap();
            fs::write(root.join(name).join("canary"), b"disposable canary").unwrap();
        }
        Self {
            root,
            executable: Path::new(env!("CARGO_BIN_EXE_slopbox-seatbelt-probe"))
                .canonicalize()
                .unwrap(),
        }
    }

    fn profile(&self, state: &str, ports: &[u16], sockets: &[&Path]) -> String {
        let mut profile = format!(
            r#"(version 1)
(deny default)
(allow process-exec process-fork)
(deny syscall-unix (syscall-number SYS_setsid SYS_setpgid))
(allow file-read*
    (literal "/")
    (literal "/System")
    (literal "/System/Volumes")
    (literal "/System/Volumes/Preboot")
    (literal "/System/Volumes/Preboot/Cryptexes")
    (subpath "/System/Volumes/Preboot/Cryptexes/OS")
    (subpath "/System/Library")
    (subpath "/usr/lib")
    (literal "/usr/bin/sandbox-exec")
    (literal "/usr/bin/true")
    (literal {}))
(allow file-read-metadata
    (literal "/var")
    (literal "/private")
    (literal "/private/var")
    (literal "/private/var/tmp"))
(allow file-read* file-write-data (literal "/dev/null"))
(allow file-read* file-write*
    (subpath {})
    (subpath {}))
(allow sysctl-read
    (sysctl-name "hw.pagesize")
    (sysctl-name "hw.pagesize_compat")
    (sysctl-name "hw.memsize")
    (sysctl-name "hw.ncpu")
    (sysctl-name "kern.osrelease")
    (sysctl-name "kern.ostype")
    (sysctl-name "kern.osversion")
    (sysctl-name "kern.argmax"))
"#,
            quoted(&self.executable),
            quoted(&self.root.join("workspace")),
            quoted(&self.root.join(state)),
        );
        for port in ports {
            profile.push_str(&format!(
                "(allow network-outbound (require-all (socket-domain AF_INET) (remote tcp \"localhost:{port}\")))\n"
            ));
        }
        if !sockets.is_empty() {
            profile.push_str("(allow system-socket (socket-domain AF_UNIX))\n");
        }
        for socket in sockets {
            profile.push_str(&format!(
                "(allow network-outbound (remote unix-socket (literal {})))\n",
                quoted(socket)
            ));
        }
        profile
    }

    fn command(&self, profile: &str) -> Command {
        let mut command = Command::new("/usr/bin/sandbox-exec");
        command
            .env_clear()
            .current_dir("/")
            .args(["-p", profile])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        command
    }

    fn supervisor(
        &self,
        ports: &[u16],
        sockets: &[&Path],
    ) -> slopbox_seatbelt_probe::supervisor::Supervisor {
        slopbox_seatbelt_probe::supervisor::Supervisor::start(
            self.executable.clone(),
            self.root.join("control/tool.sock"),
            self.profile("tool", ports, sockets),
            self.root.join("workspace"),
            self.root.join("tool"),
        )
        .unwrap()
    }

    fn tool_request(&self, profile: &str, operation: &str, target: impl AsRef<Path>) -> Output {
        finish(
            self.command(profile)
                .env("PROBE_HARNESS_MARKER", "synthetic-harness-marker")
                .arg(&self.executable)
                .arg("request")
                .arg(self.root.join("control/tool.sock"))
                .arg(&self.executable)
                .arg(operation)
                .arg(target.as_ref())
                .spawn()
                .unwrap(),
        )
    }

    fn probe(&self, profile: &str, operation: &str, target: impl AsRef<Path>) -> Output {
        let child = self
            .command(profile)
            .arg(&self.executable)
            .arg(operation)
            .arg(target.as_ref())
            .spawn()
            .unwrap();
        finish(child)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}

fn quoted(path: &Path) -> String {
    let path = path.to_str().unwrap();
    assert!(!path.chars().any(char::is_control));
    format!("\"{}\"", path.replace('\\', "\\\\").replace('"', "\\\""))
}

struct Running(Option<Child>);

impl From<Child> for Running {
    fn from(child: Child) -> Self {
        Self(Some(child))
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        if let Some(child) = &mut self.0 {
            unsafe { kill(-(child.id() as i32), SIGKILL) };
            let _ = child.wait();
        }
    }
}

fn finish(child: impl Into<Running>) -> Output {
    finish_with_timeout(child, Duration::from_secs(5))
}

fn finish_with_timeout(child: impl Into<Running>, timeout: Duration) -> Output {
    let mut child = child.into();
    let deadline = Instant::now() + timeout;
    while child.0.as_mut().unwrap().try_wait().unwrap().is_none() {
        assert!(Instant::now() < deadline, "probe timed out");
        std::thread::sleep(Duration::from_millis(5));
    }
    child.0.take().unwrap().wait_with_output().unwrap()
}

#[track_caller]
fn allowed(output: Output) {
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"allowed\n", "{output:?}");
}

#[track_caller]
fn denied(output: Output) {
    // A missing file, refused connection, crash, or timeout is not enforcement evidence.
    assert_eq!(output.status.code(), Some(77), "{output:?}");
    assert!(output.stdout.is_empty(), "{output:?}");
}

#[test]
fn fixed_harmless_command_starts() {
    let fixture = Fixture::new();
    let output = finish(
        fixture
            .command(&fixture.profile("tool", &[], &[]))
            .arg("/usr/bin/true")
            .spawn()
            .unwrap(),
    );
    assert!(output.status.success(), "{output:?}");
}

#[test]
fn filesystem_grants_exclude_unrelated_state_and_fake_credentials() {
    let fixture = Fixture::new();
    let profile = fixture.profile("tool", &[], &[]);
    for name in ["workspace", "tool"] {
        let path = fixture.root.join(name).join("canary");
        allowed(fixture.probe(&profile, "read", &path));
        allowed(fixture.probe(&profile, "write", &path));
        assert_eq!(fs::read(path).unwrap(), b"changed by probe");
    }
    for name in [".ssh", ".aws", "Keychains", "pi-auth"] {
        let path = fixture.root.join("outside").join(name);
        fs::create_dir(&path).unwrap();
        fs::write(path.join("canary"), b"fake credential").unwrap();
        denied(fixture.probe(&profile, "read", path.join("canary")));
        denied(fixture.probe(&profile, "write", path.join("canary")));
        assert_eq!(fs::read(path.join("canary")).unwrap(), b"fake credential");
    }
    for name in ["outside", "harness", "control"] {
        let path = fixture.root.join(name).join("canary");
        denied(fixture.probe(&profile, "read", &path));
        denied(fixture.probe(&profile, "write", &path));
        assert_eq!(fs::read(path).unwrap(), b"disposable canary");
    }
}

#[test]
fn symlinks_aliases_case_and_descendants_do_not_expand_the_grant() {
    let fixture = Fixture::new();
    let profile = fixture.profile("tool", &[], &[]);
    let outside = fixture.root.join("outside/canary");
    let link = fixture.root.join("workspace/link");
    symlink(&outside, &link).unwrap();
    let directory_link = fixture.root.join("workspace/directory-link");
    symlink(fixture.root.join("outside"), &directory_link).unwrap();
    let allowed_path = fixture.root.join("workspace/canary");
    let var_alias = |path: &Path| Path::new("/").join(path.strip_prefix("/private").unwrap());
    allowed(fixture.probe(&profile, "read", var_alias(&allowed_path)));
    for path in [
        outside.clone(),
        var_alias(&outside),
        link,
        directory_link.join("canary"),
    ] {
        denied(fixture.probe(&profile, "read", &path));
        denied(fixture.probe(&profile, "write", &path));
        denied(fixture.probe(&profile, "descendant-read", &path));
    }
    let case_alias = fixture.root.join("outside/CANARY");
    if case_alias.try_exists().unwrap() {
        denied(fixture.probe(&profile, "read", case_alias));
        allowed(fixture.probe(&profile, "read", fixture.root.join("workspace/CANARY")));
    } else {
        eprintln!("case-sensitive filesystem: differently cased names are not aliases");
    }
}

#[test]
fn replacing_a_granted_path_with_a_symlink_does_not_grant_its_target() {
    let fixture = Fixture::new();
    let mut child = Running::from(
        fixture
            .command(&fixture.profile("tool", &[], &[]))
            .arg(&fixture.executable)
            .arg("wait-read")
            .arg(fixture.root.join("workspace/canary"))
            .stdin(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let mut output = BufReader::new(child.0.as_mut().unwrap().stdout.take().unwrap());
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut ready = String::new();
        output.read_line(&mut ready).unwrap();
        let _ = sender.send(ready);
    });
    assert_eq!(
        receiver.recv_timeout(Duration::from_secs(5)).unwrap(),
        "ready\n"
    );
    fs::rename(
        fixture.root.join("workspace"),
        fixture.root.join("old-workspace"),
    )
    .unwrap();
    symlink(fixture.root.join("outside"), fixture.root.join("workspace")).unwrap();
    child
        .0
        .as_mut()
        .unwrap()
        .stdin
        .take()
        .unwrap()
        .write_all(b"continue\n")
        .unwrap();
    denied(finish(child));
}

#[test]
fn known_tcp_endpoints_are_role_and_session_scoped() {
    let fixture = Fixture::new();
    let model = TcpListener::bind("127.0.0.1:0").unwrap();
    let general = TcpListener::bind("127.0.0.1:0").unwrap();
    let account = TcpListener::bind("127.0.0.1:0").unwrap();
    let unrelated = TcpListener::bind("127.0.0.1:0").unwrap();
    let other_session = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = |listener: &TcpListener| listener.local_addr().unwrap();
    let harness = fixture.profile(
        "harness",
        &[
            address(&model).port(),
            address(&general).port(),
            address(&account).port(),
        ],
        &[],
    );
    let tool = fixture.profile(
        "tool",
        &[address(&general).port(), address(&account).port()],
        &[],
    );
    let no_general = fixture.profile("tool", &[address(&account).port()], &[]);
    for listener in [&model, &general, &account] {
        allowed(fixture.probe(&harness, "tcp", address(listener).to_string()));
    }
    for listener in [&general, &account] {
        allowed(fixture.probe(&tool, "tcp", address(listener).to_string()));
    }
    for listener in [&model, &unrelated, &other_session] {
        denied(fixture.probe(&tool, "tcp", address(listener).to_string()));
    }
    denied(fixture.probe(&no_general, "tcp", address(&general).to_string()));
    allowed(fixture.probe(&no_general, "tcp", address(&account).to_string()));
    denied(fixture.probe(&tool, "tcp", "192.0.2.1:443"));
    denied(fixture.probe(&tool, "tcp", "10.0.0.1:443"));
    denied(fixture.probe(
        &tool,
        "tcp",
        format!("[::ffff:127.0.0.1]:{}", address(&model).port()),
    ));
    denied(fixture.probe(&tool, "bind", "127.0.0.1:0"));
}

#[test]
fn unix_sockets_are_not_granted_by_workspace_file_access() {
    let fixture = Fixture::new();
    let model = fixture.root.join("harness/model.sock");
    let account = fixture.root.join("workspace/account.sock");
    let unrelated = fixture.root.join("workspace/host.sock");
    let _model = UnixListener::bind(&model).unwrap();
    let _account = UnixListener::bind(&account).unwrap();
    let _unrelated = UnixListener::bind(&unrelated).unwrap();
    let harness = fixture.profile("harness", &[], &[&model, &account]);
    let tool = fixture.profile("tool", &[], &[&account]);
    allowed(fixture.probe(&harness, "unix", &model));
    allowed(fixture.probe(&tool, "unix", &account));
    denied(fixture.probe(&tool, "unix", &model));
    denied(fixture.probe(&tool, "unix", &unrelated));
    let link = fixture.root.join("workspace/model-link.sock");
    symlink(&model, &link).unwrap();
    denied(fixture.probe(&tool, "unix", &link));
}

#[test]
fn inherited_descriptors_require_explicit_exec_time_closure() {
    let fixture = Fixture::new();
    let path = fixture.root.join("outside/descriptor");
    fs::write(&path, b"disposable descriptor canary").unwrap();
    let file = File::open(&path).unwrap();
    let descriptor = file.as_raw_fd();
    let profile = fixture.profile("tool", &[], &[]);
    denied(fixture.probe(&profile, "read", &path));
    for close_on_exec in [false, true] {
        let mut command = fixture.command(&profile);
        command.arg(&fixture.executable).args(["fd", "99"]);
        unsafe {
            command.pre_exec(move || {
                if dup2(descriptor, 99) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                if close_on_exec && fcntl(99, F_SETFD, FD_CLOEXEC) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let output = finish(command.spawn().unwrap());
        if close_on_exec {
            assert_eq!(output.status.code(), Some(1), "{output:?}");
            assert!(
                String::from_utf8_lossy(&output.stderr).contains("errno=Some(9)"),
                "{output:?}"
            );
        } else {
            allowed(output);
        }
    }
}

#[test]
fn tools_cannot_signal_the_host_supervisor() {
    let fixture = Fixture::new();
    denied(fixture.probe(
        &fixture.profile("tool", &[], &[]),
        "signal",
        std::process::id().to_string(),
    ));
}

#[test]
fn concurrent_sessions_have_separate_state_and_endpoint_grants() {
    let first = Fixture::new();
    let second = Fixture::new();
    let first_listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let second_listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let first_address = first_listener.local_addr().unwrap();
    let second_address = second_listener.local_addr().unwrap();
    let first_profile = first.profile("harness", &[first_address.port()], &[]);
    let second_profile = second.profile("harness", &[second_address.port()], &[]);
    std::thread::scope(|scope| {
        scope.spawn(|| {
            allowed(first.probe(&first_profile, "read", first.root.join("harness/canary")));
            denied(first.probe(&first_profile, "read", second.root.join("harness/canary")));
            allowed(first.probe(&first_profile, "tcp", first_address.to_string()));
            denied(first.probe(&first_profile, "tcp", second_address.to_string()));
        });
        scope.spawn(|| {
            allowed(second.probe(&second_profile, "read", second.root.join("harness/canary")));
            denied(second.probe(&second_profile, "read", first.root.join("harness/canary")));
            allowed(second.probe(&second_profile, "tcp", second_address.to_string()));
            denied(second.probe(&second_profile, "tcp", first_address.to_string()));
        });
    });
    drop(first);
    allowed(second.probe(&second_profile, "read", second.root.join("harness/canary")));
}

#[test]
fn localhost_port_grants_cover_both_address_families() {
    let fixture = Fixture::new();
    let ipv4 = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = ipv4.local_addr().unwrap();
    let ipv6 = TcpListener::bind(format!("[::1]:{}", address.port())).unwrap();
    let mut profile = fixture.profile("tool", &[], &[]);
    profile.push_str(&format!(
        "(allow network-outbound (remote ip \"localhost:{}\"))\n",
        address.port()
    ));
    allowed(fixture.probe(&profile, "tcp", address.to_string()));
    allowed(fixture.probe(&profile, "tcp", ipv6.local_addr().unwrap().to_string()));
}

#[test]
fn ipv4_only_tcp_grants_exclude_the_ipv6_port() {
    let fixture = Fixture::new();
    let ipv4 = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = ipv4.local_addr().unwrap();
    let ipv6 = TcpListener::bind(format!("[::1]:{}", address.port())).unwrap();
    let udp = std::net::UdpSocket::bind(address).unwrap();
    udp.set_read_timeout(Some(Duration::from_millis(500)))
        .unwrap();
    let profile = fixture.profile("tool", &[address.port()], &[]);
    allowed(fixture.probe(&profile, "tcp", address.to_string()));
    denied(fixture.probe(&profile, "tcp", ipv6.local_addr().unwrap().to_string()));
    denied(fixture.probe(
        &profile,
        "tcp",
        format!("[::ffff:127.0.0.1]:{}", address.port()),
    ));
    denied(fixture.probe(&profile, "udp", address.to_string()));
    let mut broad = fixture.profile("tool", &[], &[]);
    broad.push_str(&format!(
        "(allow network-outbound (remote ip \"localhost:{}\"))\n",
        address.port()
    ));
    allowed(fixture.probe(&broad, "udp", address.to_string()));
    let mut bytes = [0; 16];
    let (count, _) = udp.recv_from(&mut bytes).unwrap();
    assert_eq!(&bytes[..count], b"probe");
}

#[test]
fn revoked_endpoints_keep_their_port_reserved_without_granting_another_service() {
    use slopbox_seatbelt_probe::endpoint::Endpoint;
    let fixture = Fixture::new();
    let model = Endpoint::start(b"model\n").unwrap();
    let mut account = Endpoint::start(b"account\n").unwrap();
    let general = Endpoint::start(b"general\n").unwrap();
    let unrelated_ipv6 = TcpListener::bind(format!("[::1]:{}", account.address().port())).unwrap();
    let supervisor = fixture.supervisor(&[account.address().port()], &[]);
    let socket = fixture.root.join("control/tool.sock");
    let harness = fixture.profile(
        "harness",
        &[model.address().port(), account.address().port()],
        &[&socket],
    );
    let output = fixture.probe(&harness, "tcp-reply", model.address().to_string());
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"model\nallowed\n");
    for address in [
        model.address(),
        general.address(),
        unrelated_ipv6.local_addr().unwrap(),
    ] {
        let output = fixture.tool_request(&harness, "tcp-reply", address.to_string());
        assert_eq!(output.status.code(), Some(77), "{output:?}");
    }
    let output = fixture.tool_request(&harness, "tcp-reply", account.address().to_string());
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"account\nallowed\n");
    account.revoke();
    assert_eq!(
        claim_port(account.address()).unwrap_err().kind(),
        std::io::ErrorKind::AddrInUse
    );
    let output = fixture.tool_request(&harness, "tcp-reply", account.address().to_string());
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"revoked\nallowed\n");
    denied(fixture.probe(
        &harness,
        "tcp",
        unrelated_ipv6.local_addr().unwrap().to_string(),
    ));
    account.stop_serving().unwrap();
    assert_eq!(
        claim_port(account.address()).unwrap_err().kind(),
        std::io::ErrorKind::AddrInUse
    );
    drop(supervisor);
    // The endpoint lease is released only after supervised tools have stopped.
    let address = account.address();
    drop(account);
    let _reused = claim_port_eventually(address);
}

fn claim_port_eventually(address: std::net::SocketAddr) -> TcpListener {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match claim_port(address) {
            Ok(listener) => return listener,
            Err(error)
                if error.kind() == std::io::ErrorKind::AddrInUse && Instant::now() < deadline =>
            {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) => panic!("port {address} was not reusable after cleanup: {error}"),
        }
    }
}

fn claim_port(address: std::net::SocketAddr) -> std::io::Result<TcpListener> {
    use std::os::fd::FromRawFd;
    let std::net::SocketAddr::V4(address) = address else {
        panic!("expected IPv4")
    };
    let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_STREAM, 0) };
    if fd == -1 {
        return Err(std::io::Error::last_os_error());
    }
    let listener = unsafe { TcpListener::from_raw_fd(fd) };
    let enabled = 1_i32;
    // Ignore TIME_WAIT, and try the sharing options a competing server could use.
    for option in [libc::SO_REUSEADDR, libc::SO_REUSEPORT] {
        if unsafe {
            libc::setsockopt(
                fd,
                libc::SOL_SOCKET,
                option,
                (&enabled as *const i32).cast(),
                std::mem::size_of_val(&enabled) as _,
            )
        } != 0
        {
            return Err(std::io::Error::last_os_error());
        }
    }
    let address = libc::sockaddr_in {
        sin_len: std::mem::size_of::<libc::sockaddr_in>() as _,
        sin_family: libc::AF_INET as _,
        sin_port: address.port().to_be(),
        sin_addr: libc::in_addr {
            s_addr: u32::from_ne_bytes(address.ip().octets()),
        },
        sin_zero: [0; 8],
    };
    if unsafe {
        libc::bind(
            fd,
            (&address as *const libc::sockaddr_in).cast(),
            std::mem::size_of_val(&address) as _,
        )
    } != 0
        || unsafe { libc::listen(fd, 16) } != 0
    {
        return Err(std::io::Error::last_os_error());
    }
    Ok(listener)
}

#[test]
fn launchd_keeps_the_broker_port_after_its_worker_is_killed() {
    use slopbox_seatbelt_probe::coalition::{AuditToken, Identity};
    use slopbox_seatbelt_probe::job::Job;
    let fixture = Fixture::new();
    let (mut job, stream) = Job::start_with_endpoint(
        &fixture.executable,
        &fixture.root.join("control"),
        &fixture.profile("tool", &[], &[]),
        &fixture.root.join("workspace"),
        &fixture.root.join("tool"),
    )
    .unwrap();
    let address = job.endpoint().unwrap();
    assert_eq!(
        claim_port(address).unwrap_err().kind(),
        std::io::ErrorKind::AddrInUse
    );
    let mut token = AuditToken { val: [0; 8] };
    let mut size = std::mem::size_of_val(&token) as libc::socklen_t;
    assert_eq!(
        unsafe {
            libc::getsockopt(
                stream.as_raw_fd(),
                libc::SOL_LOCAL,
                libc::LOCAL_PEERTOKEN,
                (&mut token as *mut AuditToken).cast(),
                &mut size,
            )
        },
        0
    );
    let worker = Identity::read(token.val[5] as i32).unwrap();
    assert!(worker.matches(&token));
    worker.signal(libc::SIGKILL).unwrap();
    assert_reaped(FixtureProcess {
        pid: token.val[5] as i32,
        unique: worker.unique_id(),
    });
    assert_eq!(
        TcpListener::bind(address).unwrap_err().kind(),
        std::io::ErrorKind::AddrInUse
    );
    assert_eq!(
        claim_port(address).unwrap_err().kind(),
        std::io::ErrorKind::AddrInUse
    );
    job.finish().unwrap();
    let _reused = claim_port_eventually(address);
}

struct CrashHost {
    child: Child,
    control: PathBuf,
}

impl CrashHost {
    fn start(fixture: &Fixture) -> Self {
        let control = fixture.root.join("control");
        let mut profile = fixture.profile("tool", &[], &[]);
        profile.push_str("(allow network-outbound (require-all (socket-domain AF_INET) (remote tcp \"localhost:BROKER_PORT\")))\n");
        fs::write(control.join("tool.sb"), profile).unwrap();
        let child = Command::new(&fixture.executable)
            .env_clear()
            .current_dir("/")
            .args(["crash-host", fixture.root.to_str().unwrap()])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        Self { child, control }
    }

    fn ready(&mut self) -> (PathBuf, FixtureProcess, std::net::SocketAddr) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Ok(ready) = fs::read_to_string(self.control.join("ready")) {
                let fields: Vec<_> = ready.lines().collect();
                let pid = fields[1].parse().unwrap();
                let worker = slopbox_seatbelt_probe::coalition::Identity::read(pid).unwrap();
                return (
                    fields[0].into(),
                    FixtureProcess {
                        pid,
                        unique: worker.unique_id(),
                    },
                    fields[2].parse().unwrap(),
                );
            }
            assert!(
                self.child.try_wait().unwrap().is_none(),
                "crash coordinator exited early"
            );
            assert!(
                Instant::now() < deadline,
                "crash coordinator did not become ready"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn kill(&mut self) {
        if self.child.try_wait().unwrap().is_none() {
            self.child.kill().unwrap();
        }
        self.child.wait().unwrap();
    }
}

impl Drop for CrashHost {
    fn drop(&mut self) {
        self.kill();
        for entry in fs::read_dir(&self.control).unwrap() {
            let entry = entry.unwrap();
            if entry.file_name().len() == 24 {
                slopbox_seatbelt_probe::job::Job::recover(&entry.path()).unwrap();
            }
        }
    }
}

#[test]
fn hard_crashes_keep_endpoint_leases_until_fresh_process_recovery() {
    use slopbox_seatbelt_probe::coalition::Identity;
    for failure in ["host", "both", "stopped-worker"] {
        let fixture = Fixture::new();
        let mut host = CrashHost::start(&fixture);
        let (directory, worker, address) = host.ready();
        let pid_path = fixture.root.join("tool/pid");
        let tool = wait_for_pid(&pid_path);
        let identity = Identity::read(worker.pid).unwrap();
        assert_eq!(identity.unique_id(), worker.unique);
        if failure != "host" {
            identity.signal(libc::SIGSTOP).unwrap();
        }
        host.kill();
        if failure == "host" {
            assert_reaped(tool);
            assert_reaped(worker);
        } else {
            assert_eq!(Identity::read(tool.pid).unwrap().unique_id(), tool.unique);
            if failure == "both" {
                identity.signal(libc::SIGKILL).unwrap();
                assert_reaped(worker);
                // Demand may restart the worker, but its one-use controller is gone.
                let stream = std::net::TcpStream::connect(address).unwrap();
                stream.shutdown(std::net::Shutdown::Both).unwrap();
                drop(stream);
            }
        }
        assert_eq!(
            claim_port(address).unwrap_err().kind(),
            std::io::ErrorKind::AddrInUse
        );
        let output = finish(
            Command::new(&fixture.executable)
                .env_clear()
                .current_dir("/")
                .arg("recover-job")
                .arg(&directory)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .process_group(0)
                .spawn()
                .unwrap(),
        );
        assert!(output.status.success(), "{failure}: {output:?}");
        assert_reaped(tool);
        assert_reaped(worker);
        assert!(!directory.exists());
        assert_eq!(fs::read(pid_path.with_extension("runs")).unwrap(), b"run\n");
        assert!(!pid_path.with_extension("escaped").exists());
        let _reused = claim_port_eventually(address);
    }
}

#[test]
fn recovery_rejects_live_owners_and_invalid_ownership_records() {
    use slopbox_seatbelt_probe::coalition::Identity;
    let fixture = Fixture::new();
    let mut host = CrashHost::start(&fixture);
    let (directory, worker, address) = host.ready();
    let tool = wait_for_pid(&fixture.root.join("tool/pid"));
    let recover = || {
        finish(
            Command::new(&fixture.executable)
                .env_clear()
                .current_dir("/")
                .arg("recover-job")
                .arg(&directory)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .process_group(0)
                .spawn()
                .unwrap(),
        )
    };
    let output = recover();
    assert!(
        !output.status.success(),
        "recovered a live owner's job: {output:?}"
    );
    assert_eq!(Identity::read(tool.pid).unwrap().unique_id(), tool.unique);
    Identity::read(worker.pid)
        .unwrap()
        .signal(libc::SIGSTOP)
        .unwrap();
    host.kill();
    let record = fs::read_to_string(directory.join("owner")).unwrap();
    let fields: Vec<_> = record.lines().collect();
    for invalid in [
        "broken\n".to_owned(),
        format!("v1\n00000000-0000-0000-0000-000000000000\n{}\n", fields[2]),
    ] {
        fs::write(directory.join("owner"), invalid).unwrap();
        let output = recover();
        fs::write(directory.join("owner"), &record).unwrap();
        assert!(!output.status.success(), "{output:?}");
        assert_eq!(
            claim_port(address).unwrap_err().kind(),
            std::io::ErrorKind::AddrInUse
        );
        assert_eq!(Identity::read(tool.pid).unwrap().unique_id(), tool.unique);
    }
    let output = recover();
    assert!(output.status.success(), "{output:?}");
    assert_reaped(tool);
    assert_reaped(worker);
    let _reused = claim_port_eventually(address);
}

#[test]
fn crash_recovery_does_not_stop_a_second_live_session() {
    use slopbox_seatbelt_probe::coalition::Identity;
    let first = Fixture::new();
    let second = Fixture::new();
    let mut crashed = CrashHost::start(&first);
    let (directory, worker, address) = crashed.ready();
    let first_tool = wait_for_pid(&first.root.join("tool/pid"));
    let mut live = CrashHost::start(&second);
    let (other_directory, other_worker, other_address) = live.ready();
    let second_tool = wait_for_pid(&second.root.join("tool/pid"));
    Identity::read(worker.pid)
        .unwrap()
        .signal(libc::SIGSTOP)
        .unwrap();
    crashed.kill();
    let output = finish(
        Command::new(&first.executable)
            .env_clear()
            .current_dir("/")
            .arg("recover-job")
            .arg(&directory)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn()
            .unwrap(),
    );
    assert!(output.status.success(), "{output:?}");
    assert_reaped(first_tool);
    assert_reaped(worker);
    let _reused = claim_port_eventually(address);
    assert_eq!(
        Identity::read(second_tool.pid).unwrap().unique_id(),
        second_tool.unique
    );
    assert_eq!(
        Identity::read(other_worker.pid).unwrap().unique_id(),
        other_worker.unique
    );
    assert!(other_directory.is_dir());
    assert_eq!(
        claim_port(other_address).unwrap_err().kind(),
        std::io::ErrorKind::AddrInUse
    );
    drop(live);
    assert_reaped(second_tool);
    let _reused = claim_port_eventually(other_address);
}

#[test]
fn a_child_cannot_reapply_the_harness_profile() {
    let fixture = Fixture::new();
    let harness = fixture.profile("harness", &[], &[]);
    let tool = fixture.profile("tool", &[], &[]);
    allowed(fixture.probe(&harness, "read", fixture.root.join("harness/canary")));
    denied(fixture.probe(&tool, "read", fixture.root.join("harness/canary")));
    let output = finish(
        fixture
            .command(&tool)
            .args(["/usr/bin/sandbox-exec", "-p", &harness])
            .arg(&fixture.executable)
            .arg("read")
            .arg(fixture.root.join("harness/canary"))
            .spawn()
            .unwrap(),
    );
    assert_eq!(output.status.code(), Some(71), "{output:?}");
    assert!(output.stdout.is_empty(), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("sandbox_apply: Operation not permitted"),
        "{output:?}"
    );
}

#[test]
fn supervisor_launches_tools_without_harness_or_model_authority() {
    let fixture = Fixture::new();
    let model = TcpListener::bind("127.0.0.1:0").unwrap();
    let account = TcpListener::bind("127.0.0.1:0").unwrap();
    let _supervisor = fixture.supervisor(&[account.local_addr().unwrap().port()], &[]);
    let socket = fixture.root.join("control/tool.sock");
    let harness = fixture.profile("harness", &[model.local_addr().unwrap().port()], &[&socket]);
    allowed(fixture.probe(&harness, "read", fixture.root.join("harness/canary")));
    allowed(fixture.probe(&harness, "tcp", model.local_addr().unwrap().to_string()));
    allowed(fixture.tool_request(&harness, "write", fixture.root.join("workspace/canary")));
    allowed(fixture.tool_request(&harness, "write", fixture.root.join("tool/canary")));
    allowed(fixture.tool_request(&harness, "cwd", fixture.root.join("workspace")));
    allowed(fixture.tool_request(&harness, "env-missing", "PROBE_HARNESS_MARKER"));
    allowed(fixture.tool_request(&harness, "tcp", account.local_addr().unwrap().to_string()));
    for (operation, target) in [
        ("read", fixture.root.join("harness/canary")),
        ("read", fixture.root.join("control/canary")),
        ("write", fixture.root.join("outside/canary")),
        ("tcp", model.local_addr().unwrap().to_string().into()),
        ("unix", socket),
        ("setpgid", "unused".into()),
        ("signal-parent", "unused".into()),
        (
            "spawn-coalition",
            slopbox_seatbelt_probe::coalition::Coalition::read(std::process::id() as i32)
                .unwrap()
                .id()
                .to_string()
                .into(),
        ),
    ] {
        let output = fixture.tool_request(&harness, operation, &target);
        assert_eq!(
            output.status.code(),
            Some(77),
            "{operation} {target:?}: {output:?}"
        );
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("probe errno=Some(1)"),
            "{output:?}"
        );
    }
}

#[test]
fn supervisor_does_not_inherit_host_descriptors_or_accept_profile_options() {
    let fixture = Fixture::new();
    let _supervisor = fixture.supervisor(&[], &[]);
    let socket = fixture.root.join("control/tool.sock");
    let file = File::open(fixture.root.join("outside/canary")).unwrap();
    assert_eq!(unsafe { fcntl(file.as_raw_fd(), F_SETFD, 0) }, 0);
    let response = slopbox_seatbelt_probe::supervisor::request(
        &socket,
        &[
            fixture.executable.to_str().unwrap().into(),
            "fd".into(),
            file.as_raw_fd().to_string(),
        ],
    )
    .unwrap();
    assert_eq!(response.0, 1);
    assert!(String::from_utf8_lossy(&response.1).contains("errno=Some(9)"));
    let response = slopbox_seatbelt_probe::supervisor::request(
        &socket,
        &["--profile".into(), fixture.profile("harness", &[], &[])],
    )
    .unwrap();
    assert_eq!(response.0, 126);
    let response = slopbox_seatbelt_probe::supervisor::request(
        &socket,
        &[
            "/usr/bin/sandbox-exec".into(),
            "-p".into(),
            fixture.profile("harness", &[], &[]),
            fixture.executable.to_str().unwrap().into(),
            "read".into(),
            fixture.root.join("harness/canary").to_str().unwrap().into(),
        ],
    )
    .unwrap();
    assert_eq!(response.0, 71, "{response:?}");
}

#[test]
fn supervisor_rejects_bad_requests_and_bounds_output_without_replaying() {
    use std::os::unix::net::UnixStream;
    let fixture = Fixture::new();
    let _supervisor = fixture.supervisor(&[], &[]);
    let socket = fixture.root.join("control/tool.sock");
    for bytes in [0_u32.to_be_bytes(), u32::MAX.to_be_bytes()] {
        let mut stream = UnixStream::connect(&socket).unwrap();
        stream.write_all(&bytes).unwrap();
    }
    let response = slopbox_seatbelt_probe::supervisor::request(
        &socket,
        &[
            fixture.executable.to_str().unwrap().into(),
            "output".into(),
            "unused".into(),
        ],
    )
    .unwrap();
    assert_eq!(response.0, 126);
    let response =
        slopbox_seatbelt_probe::supervisor::request(&socket, &["/not-an-executable".into()])
            .unwrap();
    assert_ne!(response.0, 0);
    let harness = fixture.profile("harness", &[], &[&socket]);
    allowed(fixture.tool_request(&harness, "read", fixture.root.join("workspace/canary")));
}

#[derive(Clone, Copy)]
struct FixtureProcess {
    pid: i32,
    unique: u64,
}

fn wait_for_pid(path: &Path) -> FixtureProcess {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if let Ok(value) = fs::read_to_string(path)
            && let Some((pid, unique)) = value.split_once(' ')
        {
            return FixtureProcess {
                pid: pid.parse().unwrap(),
                unique: unique.parse().unwrap(),
            };
        }
        assert!(
            Instant::now() < deadline,
            "tool did not publish its identity"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn assert_reaped(process: FixtureProcess) {
    use slopbox_seatbelt_probe::coalition::Identity;
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let identity = match Identity::read(process.pid) {
            Ok(identity) if identity.unique_id() == process.unique => identity,
            Ok(_) => return,
            Err(error) if error.raw_os_error() == Some(libc::ESRCH) => return,
            Err(error) => panic!("cannot inspect fixture process: {error}"),
        };
        if Instant::now() >= deadline {
            // Even failure cleanup must not signal an unrelated, recycled PID.
            let _ = identity.signal(SIGKILL);
            panic!("supervisor left fixture process {} alive", process.pid);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn partial_requests_and_tool_execution_have_deadlines() {
    use std::io::Read;
    use std::os::unix::net::UnixStream;
    let fixture = Fixture::new();
    let supervisor = fixture.supervisor(&[], &[]);
    let socket = fixture.root.join("control/tool.sock");
    let mut partial = UnixStream::connect(&socket).unwrap();
    partial.write_all(&10_u32.to_be_bytes()).unwrap();
    partial.write_all(b"/").unwrap();
    partial
        .set_read_timeout(Some(Duration::from_secs(4)))
        .unwrap();
    let mut response = Vec::new();
    partial.read_to_end(&mut response).unwrap();
    assert!(response.ends_with(b"126\nrequest failed"), "{response:?}");
    let pid_path = fixture.root.join("tool/timed-pid");
    let mut stream = UnixStream::connect(&socket).unwrap();
    let body = format!(
        "{}\0sleep-write\0{}\0",
        fixture.executable.display(),
        pid_path.display()
    );
    stream
        .write_all(&(body.len() as u32).to_be_bytes())
        .unwrap();
    stream.write_all(body.as_bytes()).unwrap();
    let pid = wait_for_pid(&pid_path);
    stream
        .set_read_timeout(Some(Duration::from_secs(4)))
        .unwrap();
    let mut response = Vec::new();
    stream.read_to_end(&mut response).unwrap();
    assert!(response.ends_with(b"126\nrequest failed"), "{response:?}");
    assert_reaped(pid);
    assert!(!pid_path.with_extension("escaped").exists());
    drop(supervisor);
}

#[test]
fn disconnect_shutdown_and_leader_exit_terminate_tool_processes() {
    use std::os::unix::net::UnixStream;
    let fixture = Fixture::new();
    let socket = fixture.root.join("control/tool.sock");
    let supervisor = fixture.supervisor(&[], &[]);
    let pid_path = fixture.root.join("tool/pid");
    let mut stream = UnixStream::connect(&socket).unwrap();
    let body = format!(
        "{}\0sleep-write\0{}\0",
        fixture.executable.display(),
        pid_path.display()
    );
    stream
        .write_all(&(body.len() as u32).to_be_bytes())
        .unwrap();
    stream.write_all(body.as_bytes()).unwrap();
    let pid = wait_for_pid(&pid_path);
    drop(stream);
    assert_reaped(pid);
    fs::remove_file(&pid_path).unwrap();
    let harness = fixture.profile("harness", &[], &[&socket]);
    allowed(fixture.tool_request(&harness, "background", &pid_path));
    assert_reaped(wait_for_pid(&pid_path));
    fs::remove_file(&pid_path).unwrap();
    let mut stream = UnixStream::connect(&socket).unwrap();
    stream
        .write_all(&(body.len() as u32).to_be_bytes())
        .unwrap();
    stream.write_all(body.as_bytes()).unwrap();
    let pid = wait_for_pid(&pid_path);
    drop(supervisor);
    assert_reaped(pid);
    assert!(!socket.exists());
    assert!(!pid_path.with_extension("escaped").exists());
}

#[test]
fn supervisor_cannot_lose_descendants_to_spawned_process_groups() {
    let fixture = Fixture::new();
    let _supervisor = fixture.supervisor(&[], &[]);
    let socket = fixture.root.join("control/tool.sock");
    let harness = fixture.profile("harness", &[], &[&socket]);
    for operation in [
        "detached",
        "double-detached",
        "spawn-session",
        "forked",
        "detached-fail",
    ] {
        let pid_path = fixture.root.join("tool").join(operation);
        let output = fixture.tool_request(&harness, operation, &pid_path);
        if operation == "detached-fail" {
            assert_eq!(output.status.code(), Some(1), "{output:?}");
        } else {
            allowed(output);
        }
        let pid = wait_for_pid(&pid_path);
        assert_reaped(pid);
        if operation != "forked" {
            assert_eq!(
                fs::read_to_string(pid_path.with_extension("group")).unwrap(),
                pid.pid.to_string()
            );
        }
        if operation == "spawn-session" {
            assert_eq!(
                fs::read_to_string(pid_path.with_extension("session")).unwrap(),
                pid.pid.to_string()
            );
        }
        assert!(!pid_path.with_extension("escaped").exists());
        assert_eq!(
            fs::read_dir(fixture.root.join("control")).unwrap().count(),
            2
        );
    }
}

#[test]
fn detached_descendants_die_on_disconnect_timeout_and_shutdown() {
    use std::io::Read;
    use std::os::unix::net::UnixStream;
    for cancellation in ["disconnect", "timeout", "shutdown"] {
        let fixture = Fixture::new();
        let supervisor = fixture.supervisor(&[], &[]);
        let socket = fixture.root.join("control/tool.sock");
        let pid_path = fixture.root.join("tool/pid");
        let mut stream = UnixStream::connect(&socket).unwrap();
        let body = format!(
            "{}\0detached-wait\0{}\0",
            fixture.executable.display(),
            pid_path.display()
        );
        stream
            .write_all(&(body.len() as u32).to_be_bytes())
            .unwrap();
        stream.write_all(body.as_bytes()).unwrap();
        let pid = wait_for_pid(&pid_path);
        assert_eq!(
            fs::read_to_string(pid_path.with_extension("group")).unwrap(),
            pid.pid.to_string()
        );
        match cancellation {
            "disconnect" => {
                drop(stream);
                assert_reaped(pid);
                drop(supervisor);
            }
            "timeout" => {
                stream
                    .set_read_timeout(Some(Duration::from_secs(4)))
                    .unwrap();
                let mut response = Vec::new();
                stream.read_to_end(&mut response).unwrap();
                assert!(response.ends_with(b"126\nrequest failed"), "{response:?}");
                assert_reaped(pid);
                drop(supervisor);
            }
            "shutdown" => {
                drop(supervisor);
                assert_reaped(pid);
            }
            _ => unreachable!(),
        }
        assert!(!pid_path.with_extension("escaped").exists());
        assert_eq!(
            fs::read_dir(fixture.root.join("control")).unwrap().count(),
            1
        );
    }
}

#[test]
fn worker_connection_loss_and_worker_death_do_not_leave_tools_running() {
    use slopbox_seatbelt_probe::coalition::Identity;
    use slopbox_seatbelt_probe::job::Job;
    for worker_failure in ["connection-loss", "killed"] {
        let fixture = Fixture::new();
        let (mut job, mut stream) = Job::start(
            &fixture.executable,
            &fixture.root.join("control"),
            &fixture.profile("tool", &[], &[]),
            &fixture.root.join("workspace"),
            &fixture.root.join("tool"),
        )
        .unwrap();
        let pid_path = fixture.root.join("tool/pid");
        let body = format!(
            "{}\0detached-wait\0{}\0",
            fixture.executable.display(),
            pid_path.display()
        );
        stream
            .write_all(&(body.len() as u32).to_be_bytes())
            .unwrap();
        stream.write_all(body.as_bytes()).unwrap();
        let pid = wait_for_pid(&pid_path);
        if worker_failure == "killed" {
            let mut worker_pid = 0_i32;
            let mut size = std::mem::size_of_val(&worker_pid) as libc::socklen_t;
            assert_eq!(
                unsafe {
                    libc::getsockopt(
                        stream.as_raw_fd(),
                        libc::SOL_LOCAL,
                        libc::LOCAL_PEERPID,
                        (&mut worker_pid as *mut i32).cast(),
                        &mut size,
                    )
                },
                0
            );
            Identity::read(worker_pid)
                .unwrap()
                .signal(libc::SIGKILL)
                .unwrap();
            job.finish().unwrap();
        } else {
            drop(stream);
            // Assert before Job::finish: the worker must clean up without its client.
            assert_reaped(pid);
            job.finish().unwrap();
        }
        assert_reaped(pid);
        assert_eq!(
            fs::read_dir(fixture.root.join("control")).unwrap().count(),
            1
        );
    }
}

#[test]
fn cancellation_during_exec_waits_for_kernel_task_exit() {
    use slopbox_seatbelt_probe::job::Job;
    let fixture = Fixture::new();
    for iteration in 0..10 {
        let (mut job, mut stream) = Job::start(
            &fixture.executable,
            &fixture.root.join("control"),
            &fixture.profile("tool", &[], &[]),
            &fixture.root.join("workspace"),
            &fixture.root.join("tool"),
        )
        .unwrap();
        let pid_path = fixture.root.join("tool").join(format!("churn-{iteration}"));
        let body = format!(
            "{}\0exec-churn\0{}\0",
            fixture.executable.display(),
            pid_path.display()
        );
        stream
            .write_all(&(body.len() as u32).to_be_bytes())
            .unwrap();
        stream.write_all(body.as_bytes()).unwrap();
        let pid = wait_for_pid(&pid_path);
        job.finish().unwrap();
        assert_reaped(pid);
    }
}

#[test]
fn terminating_a_coalition_does_not_signal_another_active_job() {
    use slopbox_seatbelt_probe::job::Job;
    let first = Fixture::new();
    let second = Fixture::new();
    let mut jobs = Vec::new();
    for fixture in [&first, &second] {
        let (job, mut stream) = Job::start(
            &fixture.executable,
            &fixture.root.join("control"),
            &fixture.profile("tool", &[], &[]),
            &fixture.root.join("workspace"),
            &fixture.root.join("tool"),
        )
        .unwrap();
        let pid_path = fixture.root.join("tool/pid");
        let body = format!(
            "{}\0detached-wait\0{}\0",
            fixture.executable.display(),
            pid_path.display()
        );
        stream
            .write_all(&(body.len() as u32).to_be_bytes())
            .unwrap();
        stream.write_all(body.as_bytes()).unwrap();
        jobs.push((job, stream, wait_for_pid(&pid_path)));
    }
    jobs[0].0.finish().unwrap();
    assert_reaped(jobs[0].2);
    assert_eq!(
        slopbox_seatbelt_probe::coalition::Identity::read(jobs[1].2.pid)
            .unwrap()
            .unique_id(),
        jobs[1].2.unique
    );
    jobs[1].0.finish().unwrap();
    assert_reaped(jobs[1].2);
}

#[test]
fn concurrent_supervisors_cannot_be_reached_by_another_session_or_its_tools() {
    let first = Fixture::new();
    let second = Fixture::new();
    let first_supervisor = first.supervisor(&[], &[]);
    let _second_supervisor = second.supervisor(&[], &[]);
    let first_socket = first.root.join("control/tool.sock");
    let second_socket = second.root.join("control/tool.sock");
    let first_harness = first.profile("harness", &[], &[&first_socket]);
    let second_harness = second.profile("harness", &[], &[&second_socket]);
    std::thread::scope(|scope| {
        scope.spawn(|| {
            allowed(first.tool_request(
                &first_harness,
                "read",
                first.root.join("workspace/canary"),
            ));
            denied(first.probe(&first_harness, "unix", &second_socket));
            denied(first.probe(&first.profile("tool", &[], &[]), "unix", &first_socket));
        });
        scope.spawn(|| {
            allowed(second.tool_request(
                &second_harness,
                "read",
                second.root.join("workspace/canary"),
            ));
            denied(second.probe(&second_harness, "unix", &first_socket));
        });
    });
    drop(first_supervisor);
    allowed(second.tool_request(
        &second_harness,
        "read",
        second.root.join("workspace/canary"),
    ));
}

fn native_executable(fixture: &Fixture) -> PathBuf {
    std::env::var_os("SLOPBOX_TEST_SLOPBOX")
        .map(PathBuf::from)
        .unwrap_or_else(|| fixture.executable.clone())
        .canonicalize()
        .unwrap()
}

#[test]
#[ignore = "integration: set SLOPBOX_TEST_NODE and SLOPBOX_TEST_PI_CLI to reviewed absolute paths"]
fn native_pi_session_routes_model_tools_and_user_bash_through_the_supervisor() {
    run_native_pi(false);
}

#[test]
#[ignore = "integration: set SLOPBOX_TEST_NODE and SLOPBOX_TEST_PI_CLI to reviewed absolute paths"]
fn native_pi_session_crash_retains_leases_until_fresh_recovery() {
    run_native_pi(true);
}

#[test]
#[ignore = "integration: set SLOPBOX_TEST_NODE and SLOPBOX_TEST_PI_CLI to reviewed absolute paths"]
fn native_pi_sessions_run_concurrently() {
    std::thread::scope(|scope| {
        scope.spawn(|| run_native_pi(false));
        scope.spawn(|| run_native_pi(false));
    });
}

fn run_native_pi(crash: bool) {
    use slopbox_seatbelt_probe::engine::session::Session;
    let node =
        PathBuf::from(std::env::var_os("SLOPBOX_TEST_NODE").expect("SLOPBOX_TEST_NODE required"));
    let cli = PathBuf::from(
        std::env::var_os("SLOPBOX_TEST_PI_CLI").expect("SLOPBOX_TEST_PI_CLI required"),
    );
    assert!(node.is_absolute() && cli.is_absolute());
    let node = node.canonicalize().unwrap();
    let cli = cli.canonicalize().unwrap();
    let package = cli.parent().unwrap().parent().unwrap();
    assert!(package.join("package.json").is_file());
    let fixture = Fixture::new();
    let socket = fixture.root.join("control/native/tool.sock");
    let worker = native_executable(&fixture);
    fs::write(
        fixture.root.join("control/tool.sb"),
        fixture.profile("tool", &[], &[]) + "(allow file-read* (literal \"/bin/sh\"))\n",
    )
    .unwrap();
    let runtime = fixture.root.join("pi-runtime");
    fs::create_dir(&runtime).unwrap();
    fs::write(
        runtime.join("pi-extension.ts"),
        include_str!("../pi-extension.ts"),
    )
    .unwrap();
    fs::write(
        runtime.join("pi-session.mjs"),
        include_str!("../pi-session.mjs"),
    )
    .unwrap();
    let project = fixture.root.join("workspace/.pi");
    fs::create_dir_all(project.join("extensions")).unwrap();
    fs::write(project.join("extensions/evil.ts"),
        "import { writeFileSync } from 'node:fs'; export default function() { writeFileSync('project-extension-ran', 'unsafe'); }\n").unwrap();
    fs::write(
        project.join("settings.json"),
        "{\"defaultProjectTrust\":\"always\"}\n",
    )
    .unwrap();
    let mut profile = fixture.profile("harness", &[], &[&socket]).replace(
        "(allow process-exec process-fork)",
        &format!(
            "(allow process-fork)\n(allow process-exec (literal {}))",
            quoted(&node)
        ),
    );
    profile.push_str(
        "(allow sysctl-read (sysctl-name \"kern.hostname\" \"kern.version\" \"hw.machine\"))\n",
    );
    profile.push_str(&format!(
        "(allow file-read* (literal {}) (subpath {}) (subpath {}))\n",
        quoted(&node),
        quoted(package),
        quoted(&runtime)
    ));
    for path in node
        .ancestors()
        .skip(1)
        .chain(package.ancestors().skip(1))
        .chain(std::iter::once(fixture.root.as_path()))
    {
        profile.push_str(&format!(
            "(allow file-read-metadata (literal {}))\n",
            quoted(path)
        ));
    }
    fs::write(fixture.root.join("control/harness.sb"), profile).unwrap();
    let output = finish_with_timeout(
        Command::new(&node)
            .env_clear()
            .current_dir("/")
            .arg(runtime.join("pi-session.mjs"))
            .arg(if crash { "host-crash" } else { "host" })
            .arg(&fixture.root)
            .arg(&fixture.executable)
            .arg(&cli)
            .arg(&worker)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn()
            .unwrap(),
        Duration::from_secs(40),
    );
    let directory = fixture.root.join("control/native");
    if !output.status.success() && directory.exists() {
        Session::recover(&directory).unwrap();
    }
    assert!(output.status.success(), "{output:?}");
    assert_eq!(directory.exists(), crash);
    if crash {
        let ports: Vec<u16> = fs::read_to_string(fixture.root.join("control/native-ready"))
            .unwrap()
            .lines()
            .take(2)
            .map(|port| port.parse().unwrap())
            .collect();
        for port in &ports {
            let address = ([127, 0, 0, 1], *port).into();
            assert!(claim_port(address).is_err());
            // A live listener, not an HTTP connection's TIME_WAIT, must hold this port.
            std::net::TcpStream::connect_timeout(&address, Duration::from_millis(500)).unwrap();
        }
        let recovered = Command::new(&worker)
            .env_clear()
            .current_dir("/")
            .arg("__macos-recover")
            .arg(&directory)
            .output()
            .unwrap();
        assert!(recovered.status.success(), "{recovered:?}");
        assert!(!directory.exists());
        for port in ports {
            let error = std::net::TcpStream::connect_timeout(
                &([127, 0, 0, 1], port).into(),
                Duration::from_millis(500),
            )
            .unwrap_err();
            assert_eq!(error.kind(), std::io::ErrorKind::ConnectionRefused);
        }
    }
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("native Pi model round-trip"),
        "{output:?}"
    );
    assert_reaped(wait_for_pid(&fixture.root.join("workspace/cancel-pid")));
    assert_eq!(
        fs::read(fixture.root.join("harness/canary")).unwrap(),
        b"disposable canary"
    );
}

#[test]
#[ignore = "acceptance gate: nested sandbox_apply returns EPERM on macOS 27 build 26A428"]
fn nested_role_narrowing() {
    let fixture = Fixture::new();
    let model_listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let account_listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let model = model_listener.local_addr().unwrap();
    let account = account_listener.local_addr().unwrap();
    let mut harness = fixture.profile("harness", &[model.port(), account.port()], &[]);
    harness.push_str(&format!(
        "(allow system-mac-syscall (mac-policy-name \"Sandbox\"))\n(allow file-read* file-write* (subpath {}))\n",
        quoted(&fixture.root.join("tool")),
    ));
    let tool = fixture.profile("tool", &[account.port()], &[]);
    allowed(fixture.probe(&harness, "read", fixture.root.join("harness/canary")));
    allowed(fixture.probe(&harness, "tcp", model.to_string()));
    denied(fixture.probe(&tool, "tcp", model.to_string()));
    let nested = |profile: &str, operation: &str, target: &Path| {
        finish(
            fixture
                .command(&harness)
                .args(["/usr/bin/sandbox-exec", "-p", profile])
                .arg(&fixture.executable)
                .arg(operation)
                .arg(target)
                .spawn()
                .unwrap(),
        )
    };
    allowed(nested(
        &tool,
        "read",
        &fixture.root.join("workspace/canary"),
    ));
    allowed(nested(&tool, "write", &fixture.root.join("tool/canary")));
    denied(nested(&tool, "read", &fixture.root.join("harness/canary")));
    allowed(nested(&tool, "tcp", Path::new(&account.to_string())));
    denied(nested(&tool, "tcp", Path::new(&model.to_string())));
    let broader = fixture.profile("outside", &[], &[]);
    allowed(fixture.probe(&broader, "read", fixture.root.join("outside/canary")));
    denied(nested(
        &broader,
        "read",
        &fixture.root.join("outside/canary"),
    ));
}
