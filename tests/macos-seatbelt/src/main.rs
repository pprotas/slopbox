use std::env;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::os::fd::FromRawFd;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::process::{self, Command};
use std::time::Duration;

const F_GETFD: i32 = 1;

unsafe extern "C" {
    fn kill(pid: i32, signal: i32) -> i32;
    fn fcntl(descriptor: i32, command: i32, ...) -> i32;
}

fn probe(arguments: &[String]) -> io::Result<()> {
    let path = &arguments[1];
    match arguments[0].as_str() {
        "read" => fs::read(path).map(|_| ()),
        "write" => fs::write(path, b"changed by probe"),
        "tcp" => {
            let address: SocketAddr = path.parse().unwrap();
            TcpStream::connect_timeout(&address, Duration::from_millis(500)).map(|_| ())
        }
        "tcp-reply" => {
            let mut stream =
                TcpStream::connect_timeout(&path.parse().unwrap(), Duration::from_millis(500))?;
            stream.set_read_timeout(Some(Duration::from_millis(500)))?;
            stream.set_write_timeout(Some(Duration::from_millis(500)))?;
            stream.write_all(b"ping")?;
            stream.shutdown(std::net::Shutdown::Write)?;
            let mut response = Vec::new();
            stream.take(128).read_to_end(&mut response)?;
            io::stdout().write_all(&response)
        }
        "udp" => {
            let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM, 0) };
            if fd == -1 {
                return Err(io::Error::last_os_error());
            }
            let socket = unsafe { UdpSocket::from_raw_fd(fd) };
            socket.connect(path)?;
            socket.send(b"probe").map(|_| ())
        }
        "bind" => TcpListener::bind(path).map(|_| ()),
        "unix" => UnixStream::connect(path).map(|_| ()),
        "fd" => {
            let descriptor = path.parse().unwrap();
            if unsafe { fcntl(descriptor, F_GETFD) } == -1 {
                return Err(io::Error::last_os_error());
            }
            let mut file = unsafe { File::from_raw_fd(descriptor) };
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes)?;
            if bytes == b"disposable descriptor canary" {
                Ok(())
            } else {
                Err(io::Error::other("descriptor canary mismatch"))
            }
        }
        "setsid" => {
            if unsafe { libc::setsid() } != -1 {
                Ok(())
            } else {
                Err(io::Error::last_os_error())
            }
        }
        "setpgid" => {
            if unsafe { libc::setpgid(0, 0) } == 0 {
                Ok(())
            } else {
                Err(io::Error::last_os_error())
            }
        }
        "env-missing" => {
            if env::var_os(path).is_none() {
                Ok(())
            } else {
                Err(io::Error::other("unexpected environment variable"))
            }
        }
        "cwd" => {
            if env::current_dir()? == std::path::Path::new(path) {
                Ok(())
            } else {
                Err(io::Error::other("unexpected working directory"))
            }
        }
        "sleep-write" => {
            fs::write(
                format!("{path}.group"),
                unsafe { libc::getpgrp() }.to_string(),
            )?;
            fs::write(
                format!("{path}.session"),
                unsafe { libc::getsid(0) }.to_string(),
            )?;
            publish_pid(path)?;
            std::thread::sleep(Duration::from_secs(10));
            fs::write(format!("{path}.escaped"), b"escaped")
        }
        "background" | "detached" | "double-detached" | "detached-wait" | "detached-fail"
        | "forked" => {
            if arguments[0] == "detached-wait" {
                fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(format!("{path}.runs"))?
                    .write_all(b"run\n")?;
            }
            let mut command = Command::new(env::current_exe()?);
            if arguments[0] == "double-detached" {
                command
                    .args(["detached", path])
                    .stdout(std::process::Stdio::null());
            } else {
                command.args(["sleep-write", path]);
            }
            if arguments[0].contains("detached") {
                command.process_group(0);
            }
            if arguments[0] == "forked" {
                // Command must use fork rather than posix_spawn for pre_exec.
                unsafe {
                    command.pre_exec(|| Ok(()));
                }
            }
            let mut child = command.spawn()?;
            let deadline = std::time::Instant::now() + Duration::from_secs(1);
            while !std::path::Path::new(path).exists() {
                if std::time::Instant::now() >= deadline {
                    child.kill()?;
                    child.wait()?;
                    return Err(io::Error::other("child did not start"));
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            if arguments[0] == "double-detached" && !child.wait()?.success() {
                return Err(io::Error::other("intermediate child failed"));
            }
            if arguments[0] == "detached-wait" {
                std::thread::sleep(Duration::from_secs(10));
            }
            if arguments[0] == "detached-fail" {
                return Err(io::Error::other("deliberate leader failure"));
            }
            Ok(())
        }
        #[cfg(target_os = "macos")]
        "spawn-session" => spawn_with_attributes(path, None),
        #[cfg(target_os = "macos")]
        "spawn-coalition" => spawn_with_attributes("unused", Some(path.parse().unwrap())),
        "signal-parent" => {
            if unsafe { libc::kill(libc::getppid(), 0) } == 0 {
                Ok(())
            } else {
                Err(io::Error::last_os_error())
            }
        }
        "exec-churn" => {
            publish_pid(path)?;
            let remaining: u32 = env::var("PROBE_EXECS_LEFT")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(128)
                .min(128);
            if remaining == 0 {
                std::thread::sleep(Duration::from_secs(10));
                Ok(())
            } else {
                Err(Command::new(env::current_exe()?)
                    .env_clear()
                    .env("PROBE_EXECS_LEFT", (remaining - 1).to_string())
                    .args(["exec-churn", path])
                    .exec())
            }
        }
        "output" => io::stdout().write_all(&vec![b'x'; 64 * 1024]),
        "signal" => {
            if unsafe { kill(path.parse().unwrap(), 0) } == 0 {
                Ok(())
            } else {
                Err(io::Error::last_os_error())
            }
        }
        "wait-read" => {
            println!("ready");
            io::stdout().flush()?;
            let mut line = String::new();
            io::stdin().read_line(&mut line)?;
            fs::read(path).map(|_| ())
        }
        "descendant-read" => {
            let status = Command::new(env::current_exe()?)
                .env_clear()
                .args(["read", path])
                .status()?;
            process::exit(status.code().unwrap_or(1));
        }
        _ => Err(io::Error::other("unknown probe")),
    }
}

fn publish_pid(path: &str) -> io::Result<()> {
    #[cfg(target_os = "macos")]
    let value = format!(
        "{} {}",
        process::id(),
        slopbox_seatbelt_probe::coalition::Identity::read(process::id() as i32)?.unique_id()
    );
    #[cfg(not(target_os = "macos"))]
    let value = process::id().to_string();
    fs::write(format!("{path}.ready"), value)?;
    fs::rename(format!("{path}.ready"), path)
}

#[cfg(target_os = "macos")]
fn spawn_with_attributes(path: &str, coalition: Option<u64>) -> io::Result<()> {
    use std::ffi::CString;
    const POSIX_SPAWN_SETSID: libc::c_short = 0x0400;
    unsafe extern "C" {
        fn posix_spawnattr_setcoalition_np(
            attributes: *const libc::posix_spawnattr_t,
            coalition: u64,
            kind: i32,
            role: i32,
        ) -> i32;
    }
    let check = |error| {
        if error == 0 {
            Ok(())
        } else {
            Err(io::Error::from_raw_os_error(error))
        }
    };
    let executable = CString::new(env::current_exe()?.to_str().unwrap()).unwrap();
    let argv = [
        executable.clone(),
        CString::new("sleep-write").unwrap(),
        CString::new(path).unwrap(),
    ];
    let mut argv: Vec<_> = argv.iter().map(|value| value.as_ptr() as *mut _).collect();
    argv.push(std::ptr::null_mut());
    let mut attributes = unsafe { std::mem::zeroed() };
    check(unsafe { libc::posix_spawnattr_init(&mut attributes) })?;
    let result = (|| {
        check(unsafe { libc::posix_spawnattr_setflags(&mut attributes, POSIX_SPAWN_SETSID) })?;
        if let Some(coalition) = coalition {
            check(unsafe { posix_spawnattr_setcoalition_np(&attributes, coalition, 0, 0) })?;
        }
        let mut pid = 0;
        check(unsafe {
            libc::posix_spawn(
                &mut pid,
                executable.as_ptr(),
                std::ptr::null(),
                &attributes,
                argv.as_ptr(),
                [std::ptr::null_mut()].as_ptr(),
            )
        })?;
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        while !std::path::Path::new(path).exists() {
            if std::time::Instant::now() >= deadline {
                unsafe {
                    libc::kill(pid, libc::SIGKILL);
                    libc::waitpid(pid, std::ptr::null_mut(), 0);
                }
                return Err(io::Error::other("spawned child did not publish its PID"));
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        Ok(())
    })();
    unsafe {
        libc::posix_spawnattr_destroy(&mut attributes);
    }
    result
}

#[cfg(target_os = "macos")]
fn crash_host(root: &std::path::Path) -> io::Result<()> {
    use slopbox_seatbelt_probe::job::Job;
    let executable = env::current_exe()?;
    let (mut job, mut stream) = Job::start_with_endpoint(
        &executable,
        &root.join("control"),
        &fs::read_to_string(root.join("control/tool.sb"))?,
        &root.join("workspace"),
        &root.join("tool"),
    )?;
    let body = format!(
        "{}\0detached-wait\0{}\0",
        executable.display(),
        root.join("tool/pid").display()
    );
    stream.write_all(&(body.len() as u32).to_be_bytes())?;
    stream.write_all(body.as_bytes())?;
    let mut worker_pid = 0_i32;
    let mut size = std::mem::size_of_val(&worker_pid) as libc::socklen_t;
    if unsafe {
        libc::getsockopt(
            std::os::fd::AsRawFd::as_raw_fd(&stream),
            libc::SOL_LOCAL,
            libc::LOCAL_PEERPID,
            (&mut worker_pid as *mut i32).cast(),
            &mut size,
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    let ready = format!(
        "{}\n{worker_pid}\n{}\n",
        job.directory().display(),
        job.endpoint().unwrap()
    );
    fs::write(root.join("control/ready.tmp"), ready)?;
    fs::rename(root.join("control/ready.tmp"), root.join("control/ready"))?;
    let mut byte = [0];
    let _count = io::stdin().read(&mut byte)?;
    drop(stream);
    job.finish()
}

#[cfg(target_os = "macos")]
fn native_session(arguments: &[String], pi: bool, crash: bool) -> io::Result<i32> {
    use slopbox_seatbelt_probe::engine::{
        relay::Routes,
        session::{Role, Session},
    };
    let root = std::path::Path::new(&arguments[0]);
    let worker = std::path::PathBuf::from(&arguments[1]);
    let mut session = Session::start(
        worker.clone(),
        root.join("control/native"),
        Routes {
            general: (!pi).then(|| root.join("control/general.sock")),
            model: Some(root.join("control/model.sock")),
            account: Some(root.join("control/account.sock")),
        },
    )?;
    let ports = session.ports()?;
    let account = ports.account.unwrap();
    let model = ports.model.unwrap();
    session.start_tools(Role {
        profile: fs::read_to_string(root.join("control/tool.sb"))? + &format!(
            "\n(allow network-outbound (require-all (socket-domain AF_INET) (remote tcp \"localhost:{account}\")))\n"),
        workspace: root.join("workspace"), home: root.join("tool"),
    })?;
    fs::write(
        root.join("control/native-ready.tmp"),
        format!("{model}\n{account}\n{}\n", ports.general.unwrap_or(0)),
    )?;
    fs::rename(
        root.join("control/native-ready.tmp"),
        root.join("control/native-ready"),
    )?;
    let status = if pi {
        let profile = fs::read_to_string(root.join("control/harness.sb"))?
            + &format!(
                "\n(allow network-outbound (require-all (socket-domain AF_INET) (remote tcp \"localhost:{model}\")))\n"
            );
        let (status, output) = session.run_harness(
            &Role {
                profile,
                workspace: root.join("workspace"),
                home: root.join("harness"),
            },
            &[
                arguments[2].clone(),
                root.join("pi-runtime/pi-session.mjs")
                    .to_str()
                    .unwrap()
                    .into(),
                if crash { "guest-crash" } else { "guest" }.into(),
                arguments[0].clone(),
                env::current_exe()?.to_str().unwrap().into(),
                arguments[3].clone(),
                worker.to_str().unwrap().into(),
                format!("127.0.0.1:{account}"),
                model.to_string(),
            ],
        )?;
        io::stdout().write_all(&output)?;
        status
    } else {
        let mut byte = [0];
        while io::stdin().read(&mut byte)? != 0 && byte != *b"Q" {
            if byte != *b"R" {
                return Err(io::Error::other("invalid host command"));
            }
            session.revoke()?;
            fs::write(root.join("control/revoked"), b"revoked")?;
        }
        0
    };
    session.finish()?;
    Ok(status)
}

fn main() {
    let arguments: Vec<_> = env::args().skip(1).collect();
    #[cfg(target_os = "macos")]
    if (arguments
        .first()
        .is_some_and(|argument| matches!(argument.as_str(), "run-session" | "run-session-crash"))
        && arguments.len() == 5)
        || (arguments
            .first()
            .is_some_and(|argument| argument == "session-host")
            && arguments.len() == 3)
    {
        match native_session(
            &arguments[1..],
            arguments[0] != "session-host",
            arguments[0] == "run-session-crash",
        ) {
            Ok(status) => process::exit(status),
            Err(error) => {
                eprintln!("native session failed: {error}");
                process::exit(1);
            }
        }
    }
    #[cfg(target_os = "macos")]
    if arguments
        .first()
        .is_some_and(|argument| argument == "__macos-relay-worker")
        && arguments.len() == 5
    {
        if let Err(error) = slopbox_seatbelt_probe::engine::relay::worker(
            std::path::Path::new(&arguments[1]),
            [2, 3, 4].map(|index| {
                (!arguments[index].is_empty()).then(|| arguments[index].clone().into())
            }),
        ) {
            eprintln!("relay failed: {error}");
            process::exit(1);
        }
        return;
    }
    #[cfg(target_os = "macos")]
    if arguments
        .first()
        .is_some_and(|argument| argument == "__macos-recover")
        && arguments.len() == 2
    {
        if let Err(error) = slopbox_seatbelt_probe::engine::session::Session::recover(
            std::path::Path::new(&arguments[1]),
        ) {
            eprintln!("recovery failed: {error}");
            process::exit(1);
        }
        return;
    }
    #[cfg(target_os = "macos")]
    if arguments.first().is_some_and(|argument| {
        matches!(argument.as_str(), "__macos-exec-worker" | "leased-worker")
    }) && arguments.len() == 6
    {
        let result = (|| {
            let mut stream = slopbox_seatbelt_probe::native_job::connect_worker(
                std::path::Path::new(&arguments[1]),
            )?;
            let endpoint = (arguments[0] == "leased-worker")
                .then(|| slopbox_seatbelt_probe::endpoint::Endpoint::activate(b"account\n"))
                .transpose()?;
            let profile = if let Some(endpoint) = &endpoint {
                stream.write_all(&endpoint.address().port().to_be_bytes())?;
                arguments[2].replace("BROKER_PORT", &endpoint.address().port().to_string())
            } else {
                arguments[2].clone()
            };
            slopbox_seatbelt_probe::supervisor::worker_stream(
                &mut stream,
                &profile,
                std::path::Path::new(&arguments[3]),
                std::path::Path::new(&arguments[4]),
                Duration::from_secs(arguments[5].parse().unwrap()),
            )
        })();
        if let Err(error) = result {
            eprintln!("worker failed: {error}");
            process::exit(1);
        }
        return;
    }
    #[cfg(target_os = "macos")]
    if arguments
        .first()
        .is_some_and(|argument| argument == "request")
        && arguments.len() >= 3
    {
        match slopbox_seatbelt_probe::supervisor::request(
            std::path::Path::new(&arguments[1]),
            &arguments[2..],
        ) {
            Ok((status, output)) => {
                io::stdout().write_all(&output).unwrap();
                process::exit(status);
            }
            Err(error) => {
                eprintln!("request failed: {error}");
                process::exit(if error.kind() == io::ErrorKind::PermissionDenied {
                    77
                } else {
                    1
                });
            }
        }
    }
    #[cfg(target_os = "macos")]
    if arguments.len() == 2 && matches!(arguments[0].as_str(), "crash-host" | "recover-job") {
        let path = std::path::Path::new(&arguments[1]);
        let result = if arguments[0] == "crash-host" {
            crash_host(path)
        } else {
            slopbox_seatbelt_probe::job::Job::recover(path)
        };
        if let Err(error) = result {
            eprintln!("{} failed: {error}", arguments[0]);
            process::exit(1);
        }
        return;
    }
    if arguments.len() != 2 {
        eprintln!(
            "usage: slopbox-seatbelt-probe <operation> <target> | request <socket> <program> [arguments...]"
        );
        process::exit(2);
    }
    match probe(&arguments) {
        Ok(()) => println!("allowed"),
        Err(error) => {
            eprintln!("probe errno={:?}: {error}", error.raw_os_error());
            process::exit(if error.kind() == io::ErrorKind::PermissionDenied {
                77
            } else {
                1
            });
        }
    }
}
