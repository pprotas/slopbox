use super::*;
use slopbox_seatbelt_probe::engine::{
    coalition::{Coalition, Identity},
    relay::Routes,
    session::Session,
};
use std::io::Read;
use std::net::{Shutdown, SocketAddr, TcpStream};
use std::os::unix::net::UnixStream;

struct Host {
    child: Child,
    directory: PathBuf,
    ports: [u16; 3],
}

impl Host {
    fn start(fixture: &Fixture) -> Self {
        fs::write(
            fixture.root.join("control/tool.sb"),
            fixture.profile("tool", &[], &[]),
        )
        .unwrap();
        let child = Command::new(&fixture.executable)
            .env_clear()
            .current_dir("/")
            .args([
                "session-host",
                fixture.root.to_str().unwrap(),
                native_executable(fixture).to_str().unwrap(),
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let ready = fixture.root.join("control/native-ready");
        let mut host = Self {
            child,
            directory: fixture.root.join("control/native"),
            ports: [0; 3],
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        while !ready.exists() {
            assert!(
                host.child.try_wait().unwrap().is_none(),
                "native host exited"
            );
            assert!(Instant::now() < deadline, "native host startup timeout");
            std::thread::sleep(Duration::from_millis(5));
        }
        host.ports = fs::read_to_string(ready)
            .unwrap()
            .lines()
            .map(|port| port.parse().unwrap())
            .collect::<Vec<_>>()
            .try_into()
            .unwrap();
        assert!(host.ports.iter().all(|port| *port != 0));
        host
    }

    fn recover(&self, fixture: &Fixture) -> Output {
        Command::new(native_executable(fixture))
            .env_clear()
            .current_dir("/")
            .arg("__macos-recover")
            .arg(&self.directory)
            .output()
            .unwrap()
    }

    fn tool(&self, fixture: &Fixture, operation: &str, path: &Path) -> UnixStream {
        let mut stream = UnixStream::connect(self.directory.join("tool.sock")).unwrap();
        let body = format!(
            "{}\0{operation}\0{}\0",
            fixture.executable.display(),
            path.display()
        );
        stream
            .write_all(&(body.len() as u32).to_be_bytes())
            .unwrap();
        stream.write_all(body.as_bytes()).unwrap();
        stream
    }

    fn crash(&mut self) {
        self.child.kill().unwrap();
        assert!(!self.child.wait().unwrap().success());
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if self.directory.exists() {
            Session::recover(&self.directory).unwrap();
        }
    }
}

fn worker(jobs: &Path) -> Identity {
    let entries: Vec<_> = fs::read_dir(jobs)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(entries.len(), 1);
    let owner = fs::read_to_string(entries[0].join("owner")).unwrap();
    let coalition: u64 = owner.lines().last().unwrap().parse().unwrap();
    let mut pids = vec![0_i32; 128 * 1024];
    let count =
        unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast(), (pids.len() * 4) as i32) };
    assert!(count > 0 && (count as usize) < pids.len());
    for pid in &pids[..count as usize] {
        let Ok(identity) = Identity::read(*pid) else {
            continue;
        };
        if Coalition::read(*pid).map(|value| value.id()).ok() != Some(coalition) {
            continue;
        }
        let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of_val(&info) as i32;
        if unsafe {
            libc::proc_pidinfo(
                *pid,
                libc::PROC_PIDTBSDINFO,
                0,
                (&mut info as *mut libc::proc_bsdinfo).cast(),
                size,
            )
        } == size
            && info.pbi_ppid == 1
        {
            return identity;
        }
    }
    panic!("launchd worker not found");
}

#[test]
fn crash_recovery_reaps_roles_before_releasing_all_broker_ports() {
    for kill_workers in [false, true] {
        let fixture = Fixture::new();
        let mut host = Host::start(&fixture);
        let pid_file = fixture.root.join("workspace/native-pid");
        let _request = host.tool(&fixture, "detached-wait", &pid_file);
        let descendant = wait_for_pid(&pid_file);
        let tool = worker(&host.directory.join("tasks"));
        let relay = worker(&host.directory.join("leases"));
        tool.signal(libc::SIGSTOP).unwrap();
        host.crash();
        if kill_workers {
            tool.signal(libc::SIGKILL).unwrap();
            relay.signal(libc::SIGKILL).unwrap();
        }
        assert_eq!(
            Identity::read(descendant.pid).unwrap().unique_id(),
            descendant.unique
        );
        for port in host.ports {
            let address: SocketAddr = ([127, 0, 0, 1], port).into();
            assert!(
                claim_port(address).is_err(),
                "lease released while a role survives"
            );
            let _ = TcpStream::connect_timeout(&address, Duration::from_millis(200));
        }
        let output = host.recover(&fixture);
        assert!(output.status.success(), "{output:?}");
        assert_reaped(descendant);
        assert!(!host.directory.exists());
        for port in host.ports {
            claim_port_eventually(([127, 0, 0, 1], port).into());
        }
    }
}

#[test]
fn uncertain_task_ownership_retains_every_lease_and_other_sessions_keep_running() {
    let first = Fixture::new();
    let second = Fixture::new();
    let mut crashed = Host::start(&first);
    let mut live = Host::start(&second);
    assert!(
        !crashed.recover(&first).status.success(),
        "recovered a live session"
    );
    let pid_file = first.root.join("workspace/native-pid");
    let _request = crashed.tool(&first, "detached-wait", &pid_file);
    let descendant = wait_for_pid(&pid_file);
    worker(&crashed.directory.join("tasks"))
        .signal(libc::SIGSTOP)
        .unwrap();
    let task = fs::read_dir(crashed.directory.join("tasks"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let owner_path = task.join("owner");
    let owner = fs::read(&owner_path).unwrap();
    crashed.crash();
    fs::write(&owner_path, b"invalid\n").unwrap();
    let rejected = crashed.recover(&first);
    assert!(!rejected.status.success(), "{rejected:?}");
    assert!(crashed.directory.exists());
    for port in crashed.ports {
        assert!(claim_port(([127, 0, 0, 1], port).into()).is_err());
    }
    fs::write(&owner_path, owner).unwrap();
    let output = crashed.recover(&first);
    assert!(output.status.success(), "{output:?}");
    assert_reaped(descendant);
    assert!(live.child.try_wait().unwrap().is_none());
    let result = slopbox_seatbelt_probe::supervisor::request(
        &live.directory.join("tool.sock"),
        &[
            second.executable.to_str().unwrap().into(),
            "write".into(),
            second.root.join("workspace/alive").to_str().unwrap().into(),
        ],
    )
    .unwrap();
    assert_eq!(result.0, 0);
    for port in live.ports {
        assert!(claim_port(([127, 0, 0, 1], port).into()).is_err());
    }
}

#[test]
fn startup_recovers_stale_sessions_but_never_overwrites_live_or_invalid_owners() {
    let fixture = Fixture::new();
    let mut host = Host::start(&fixture);
    assert!(
        Session::start(
            native_executable(&fixture),
            host.directory.clone(),
            Routes::default()
        )
        .is_err()
    );
    host.crash();
    let owner = host.directory.join("owner");
    let valid = fs::read(&owner).unwrap();
    fs::write(&owner, "invalid").unwrap();
    let error = Session::start(
        native_executable(&fixture),
        host.directory.clone(),
        Routes::default(),
    )
    .err()
    .unwrap();
    assert!(error.to_string().contains("invalid native session record"));
    for port in host.ports {
        assert!(claim_port(([127, 0, 0, 1], port).into()).is_err());
    }
    fs::write(owner, valid).unwrap();
    let mut session = Session::start(
        native_executable(&fixture),
        host.directory.clone(),
        Routes::default(),
    )
    .unwrap();
    for port in host.ports {
        claim_port_eventually(([127, 0, 0, 1], port).into());
    }
    assert!(session.ports().unwrap().model.is_none());
    session.finish().unwrap();
}

#[test]
fn relays_stream_with_backpressure_and_preserve_half_closes() {
    let fixture = Fixture::new();
    let path = fixture.root.join("control/general.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let broker = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut bytes = Vec::new();
        stream.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, vec![b'x'; 1024 * 1024]);
        stream.write_all(&vec![b'y'; 1024 * 1024]).unwrap();
    });
    let mut session = Session::start(
        native_executable(&fixture),
        fixture.root.join("control/native"),
        Routes {
            general: Some(path),
            ..Routes::default()
        },
    )
    .unwrap();
    let port = session.ports().unwrap().general.unwrap();
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream.write_all(&vec![b'x'; 1024 * 1024]).unwrap();
    stream.shutdown(Shutdown::Write).unwrap();
    let mut bytes = Vec::new();
    stream.read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, vec![b'y'; 1024 * 1024]);
    broker.join().unwrap();
    session.finish().unwrap();
    claim_port_eventually(([127, 0, 0, 1], port).into());
}

#[test]
fn revocation_closes_live_streams_without_releasing_role_scoped_ports() {
    let fixture = Fixture::new();
    let paths = ["general", "model", "account"]
        .map(|name| fixture.root.join(format!("control/{name}.sock")));
    let brokers = paths
        .iter()
        .enumerate()
        .map(|(index, path)| {
            let listener = UnixListener::bind(path).unwrap();
            std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                stream.write_all(&[index as u8]).unwrap();
                let mut byte = [0];
                assert_eq!(
                    stream.read(&mut byte).unwrap(),
                    0,
                    "revoked relay remains open upstream"
                );
            })
        })
        .collect::<Vec<_>>();
    let mut session = Session::start(
        native_executable(&fixture),
        fixture.root.join("control/native"),
        Routes {
            general: Some(paths[0].clone()),
            model: Some(paths[1].clone()),
            account: Some(paths[2].clone()),
        },
    )
    .unwrap();
    let ports = session.ports().unwrap();
    let ports = [
        ports.general.unwrap(),
        ports.model.unwrap(),
        ports.account.unwrap(),
    ];
    let mut streams = ports
        .into_iter()
        .enumerate()
        .map(|(index, port)| {
            let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut byte = [0];
            stream.read_exact(&mut byte).unwrap();
            assert_eq!(byte, [index as u8], "broker routes crossed");
            stream
        })
        .collect::<Vec<_>>();
    let profile = fixture.profile("tool", &[ports[2]], &[]);
    allowed(fixture.probe(&profile, "tcp", format!("127.0.0.1:{}", ports[2])));
    denied(fixture.probe(&profile, "tcp", format!("127.0.0.1:{}", ports[1])));
    denied(fixture.probe(&profile, "tcp", format!("127.0.0.1:{}", ports[0])));
    session.revoke().unwrap();
    for mut stream in streams.drain(..) {
        assert_eq!(stream.read(&mut [0]).unwrap(), 0);
        // Reset the client half-close so TIME_WAIT cannot masquerade as a lease.
        let linger = libc::linger {
            l_onoff: 1,
            l_linger: 0,
        };
        assert_eq!(
            unsafe {
                libc::setsockopt(
                    stream.as_raw_fd(),
                    libc::SOL_SOCKET,
                    libc::SO_LINGER,
                    (&linger as *const libc::linger).cast(),
                    std::mem::size_of_val(&linger) as _,
                )
            },
            0
        );
    }
    for broker in brokers {
        broker.join().unwrap();
    }
    for port in ports {
        assert!(claim_port(([127, 0, 0, 1], port).into()).is_err());
    }
    session.finish().unwrap();
    for port in ports {
        claim_port_eventually(([127, 0, 0, 1], port).into());
    }
}
