use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use super::coalition::{AuditToken, Coalition, Identity};

pub struct Job {
    directory: PathBuf,
    service: String,
    coalition: Option<Coalition>,
    loaded: bool,
    cleanup_on_drop: bool,
    _lock: File,
}

impl Job {
    pub fn start(
        executable: &Path,
        control: &Path,
        worker: &str,
        arguments: &[String],
        sockets: &[&str],
    ) -> io::Result<(Self, UnixStream)> {
        if !executable.is_absolute()
            || sockets.len() > 3
            || sockets.iter().any(|name| {
                name.is_empty() || !name.bytes().all(|byte| byte.is_ascii_alphanumeric())
            })
        {
            return Err(io::Error::other("invalid native worker configuration"));
        }
        let mut random = [0_u8; 12];
        unsafe { libc::arc4random_buf(random.as_mut_ptr().cast(), random.len()) };
        let id: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
        let directory = control.join(&id);
        DirBuilder::new().mode(0o700).create(&directory)?;
        let label = format!("dev.slopbox.native.{id}");
        let domain = format!("gui/{}", unsafe { libc::getuid() });
        let lock = lock(&directory, true)?;
        let mut job = Self {
            directory,
            service: format!("{domain}/{label}"),
            coalition: None,
            loaded: false,
            cleanup_on_drop: true,
            _lock: lock,
        };
        job.record()?;
        let socket = job.directory.join("c");
        let listener = UnixListener::bind(&socket)?;
        listener.set_nonblocking(true)?;
        let prefix = [
            executable
                .to_str()
                .ok_or_else(|| io::Error::other("non-UTF-8 executable"))?,
            worker,
            socket
                .to_str()
                .ok_or_else(|| io::Error::other("non-UTF-8 control path"))?,
        ];
        let arguments: String = prefix
            .into_iter()
            .chain(arguments.iter().map(String::as_str))
            .map(|argument| format!("<string>{}</string>", xml(argument)))
            .collect();
        // LaunchOnlyOnce retires sockets on exit. Replacement workers cannot
        // receive a request through the closed, one-use control listener.
        let launch_once = if sockets.is_empty() { "true" } else { "false" };
        let sockets: String = sockets
            .iter()
            .map(|name| {
                format!(
                    "<key>{name}</key><dict>\
             <key>SockFamily</key><string>IPv4</string>\
             <key>SockType</key><string>stream</string>\
             <key>SockProtocol</key><string>TCP</string>\
             <key>SockNodeName</key><string>127.0.0.1</string>\
             <key>SockServiceName</key><integer>0</integer>\
             </dict>"
                )
            })
            .collect();
        let sockets = if sockets.is_empty() {
            sockets
        } else {
            format!("<key>Sockets</key><dict>{sockets}</dict>")
        };
        let plist = job.directory.join("job.plist");
        fs::write(
            &plist,
            format!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
                 <plist version=\"1.0\"><dict>\
                 <key>Label</key><string>{label}</string>\
                 <key>ProgramArguments</key><array>{arguments}</array>\
                 <key>RunAtLoad</key><true/>\
                 <key>KeepAlive</key><false/>\
                 <key>LaunchOnlyOnce</key><{launch_once}/>\
                 <key>ExitTimeOut</key><integer>1</integer>\
                 {sockets}</dict></plist>"
            ),
        )?;
        // A failed bootstrap can still have registered the job.
        job.loaded = true;
        launchctl(&["bootstrap", &domain, plist.to_str().unwrap()], false)?;
        let deadline = Instant::now() + Duration::from_secs(2);
        let stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    if Instant::now() >= deadline {
                        return Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            "job did not connect",
                        ));
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(error) => return Err(error),
            }
        };
        let mut token = AuditToken { val: [0; 8] };
        let mut size = std::mem::size_of_val(&token) as libc::socklen_t;
        if unsafe {
            libc::getsockopt(
                stream.as_raw_fd(),
                libc::SOL_LOCAL,
                libc::LOCAL_PEERTOKEN,
                (&mut token as *mut AuditToken).cast(),
                &mut size,
            )
        } != 0
        {
            return Err(io::Error::last_os_error());
        }
        if size as usize != std::mem::size_of_val(&token) {
            return Err(io::Error::other("invalid peer token size"));
        }
        let coalition = Coalition::read(token.val[5] as i32)?;
        if !Identity::read(token.val[5] as i32)?.matches(&token)
            || !coalition.is_distinct_from(Coalition::read(std::process::id() as i32)?)
            || coalition.live_tasks()? != 1
        {
            return Err(io::Error::other("job does not own an isolated coalition"));
        }
        job.coalition = Some(coalition);
        // Publish ownership before the caller can send an executable request.
        job.record()?;
        stream.set_nonblocking(true)?;
        Ok((job, stream))
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }

    pub fn retain_on_drop(&mut self) {
        self.cleanup_on_drop = false;
    }

    fn record(&self) -> io::Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(self.directory.join("owner.tmp"))?;
        writeln!(
            file,
            "v1\n{}\n{}",
            boot_uuid()?,
            self.coalition
                .map_or_else(|| "pending".into(), |coalition| coalition.id().to_string())
        )?;
        file.sync_all()?;
        fs::rename(
            self.directory.join("owner.tmp"),
            self.directory.join("owner"),
        )?;
        File::open(&self.directory)?.sync_all()
    }

    pub fn recover(directory: &Path) -> io::Result<()> {
        let id = directory
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|id| id.len() == 24 && id.bytes().all(|byte| byte.is_ascii_hexdigit()))
            .ok_or_else(|| io::Error::other("invalid job directory"))?;
        let metadata = fs::symlink_metadata(directory)?;
        if !metadata.is_dir()
            || metadata.uid() != unsafe { libc::getuid() }
            || metadata.mode() & 0o077 != 0
        {
            return Err(io::Error::other("job directory is not private"));
        }
        let lock = lock(directory, false)?;
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(directory.join("owner"))?;
        let metadata = file.metadata()?;
        if !metadata.is_file()
            || metadata.uid() != unsafe { libc::getuid() }
            || metadata.mode() & 0o077 != 0
            || metadata.len() > 256
        {
            return Err(io::Error::other("invalid ownership record"));
        }
        let mut record = String::new();
        file.take(257).read_to_string(&mut record)?;
        let fields: Vec<_> = record.lines().collect();
        if fields.len() != 3 || fields[0] != "v1" || fields[1] != boot_uuid()? {
            return Err(io::Error::other(
                "invalid ownership record or different boot",
            ));
        }
        let coalition = if fields[2] == "pending" {
            None
        } else {
            Some(Coalition::restore(
                fields[2].parse().map_err(io::Error::other)?,
            )?)
        };
        let mut job = Self {
            directory: directory.to_owned(),
            service: format!("gui/{}/dev.slopbox.native.{id}", unsafe { libc::getuid() }),
            coalition,
            loaded: true,
            cleanup_on_drop: true,
            _lock: lock,
        };
        job.finish()
    }

    pub fn finish(&mut self) -> io::Result<()> {
        if let Some(coalition) = self.coalition {
            coalition.terminate(None)?;
            self.coalition = None;
        }
        if self.loaded {
            launchctl(&["bootout", &self.service], true)?;
            self.loaded = false;
        }
        if self.directory.exists() {
            fs::remove_dir_all(&self.directory)?;
        }
        Ok(())
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        if self.cleanup_on_drop
            && let Err(error) = self.finish()
        {
            eprintln!("native job cleanup failed for {}: {error}", self.service);
        }
    }
}

pub fn connect_worker(socket: &Path) -> io::Result<UnixStream> {
    if unsafe { libc::getppid() } != 1 {
        return Err(io::Error::other(
            "native workers require a launchd-owned job",
        ));
    }
    private_directory(
        socket
            .parent()
            .ok_or_else(|| io::Error::other("invalid worker socket"))?,
    )?;
    UnixStream::connect(socket)
}

pub(super) fn private_directory(directory: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(directory)?;
    if !metadata.is_dir()
        || metadata.uid() != unsafe { libc::getuid() }
        || metadata.mode() & 0o077 != 0
    {
        return Err(io::Error::other("native control directory is not private"));
    }
    Ok(())
}

pub(super) fn lock(directory: &Path, create: bool) -> io::Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(create)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(directory.join("lock"))?;
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.uid() != unsafe { libc::getuid() }
        || metadata.mode() & 0o077 != 0
    {
        return Err(io::Error::other("invalid job lock"));
    }
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(file)
}

pub(super) fn boot_uuid() -> io::Result<String> {
    let mut bytes = [0_u8; 37];
    let mut size = bytes.len();
    if unsafe {
        libc::sysctlbyname(
            c"kern.bootsessionuuid".as_ptr(),
            bytes.as_mut_ptr().cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    if size != bytes.len() || bytes[36] != 0 {
        return Err(io::Error::other("invalid boot UUID"));
    }
    String::from_utf8(bytes[..36].to_vec()).map_err(io::Error::other)
}

fn launchctl(arguments: &[&str], missing_ok: bool) -> io::Result<()> {
    let mut child = Command::new("/bin/launchctl")
        .args(arguments)
        .env_clear()
        .current_dir("/")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()?;
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if let Some(status) = child.try_wait()? {
            if status.success() || (missing_ok && status.code() == Some(3)) {
                return Ok(());
            }
            let output = child.wait_with_output()?;
            return Err(io::Error::other(format!(
                "launchctl {} failed: {}",
                arguments[0],
                String::from_utf8_lossy(&output.stderr)
            )));
        }
        if Instant::now() >= deadline {
            child.kill()?;
            child.wait()?;
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "launchctl timed out",
            ));
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}
