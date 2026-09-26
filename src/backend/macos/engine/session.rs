use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::job::{Job, boot_uuid, lock, private_directory};
use super::relay::{Lease, Ports, Routes};
use super::supervisor::{MAX_OUTPUT, MAX_REQUEST, Supervisor, path_text};

pub struct Role {
    pub profile: String,
    pub workspace: PathBuf,
    pub home: PathBuf,
}

pub struct Session {
    directory: PathBuf,
    executable: PathBuf,
    tools: Option<Supervisor>,
    lease: Option<Lease>,
    finished: bool,
    _lock: File,
}

impl Session {
    pub fn start(executable: PathBuf, directory: PathBuf, routes: Routes) -> io::Result<Self> {
        if !executable.is_absolute() || !directory.is_absolute() {
            return Err(io::Error::other("native session paths must be absolute"));
        }
        if directory.try_exists()? {
            Self::recover(&directory)?;
        }
        DirBuilder::new().mode(0o700).create(&directory)?;
        let lock = lock(&directory, true)?;
        let mut session = Self {
            executable,
            directory,
            tools: None,
            lease: None,
            finished: false,
            _lock: lock,
        };
        let mut marker = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(session.directory.join("owner"))?;
        writeln!(marker, "native-v1\n{}", boot_uuid()?)?;
        marker.sync_all()?;
        for name in ["tasks", "leases"] {
            DirBuilder::new()
                .mode(0o700)
                .create(session.directory.join(name))?;
        }
        File::open(&session.directory)?.sync_all()?;
        session.lease = Some(Lease::start(
            &session.executable,
            &session.directory.join("leases"),
            routes,
        )?);
        Ok(session)
    }

    pub fn ports(&self) -> io::Result<Ports> {
        self.lease
            .as_ref()
            .map(|lease| lease.ports)
            .ok_or_else(|| io::Error::other("session is stopped"))
    }

    pub fn tool_socket(&self) -> PathBuf {
        self.directory.join("tool.sock")
    }

    pub fn start_tools(&mut self, role: Role) -> io::Result<()> {
        if self.tools.is_some() || self.lease.is_none() {
            return Err(io::Error::other("invalid native tool supervisor state"));
        }
        self.tools = Some(Supervisor::start_in(
            self.executable.clone(),
            self.tool_socket(),
            self.directory.join("tasks"),
            role.profile,
            role.workspace,
            role.home,
        )?);
        Ok(())
    }

    pub fn start_tools_configured(
        &mut self,
        role: Role,
        environment: &[String],
        timeout: Duration,
        command_prefix: Vec<String>,
    ) -> io::Result<()> {
        if self.tools.is_some() || self.lease.is_none() {
            return Err(io::Error::other("invalid tool supervisor state"));
        }
        let environment = self.write_environment("tool.env", environment)?;
        self.tools = Some(Supervisor::start_configured(
            self.executable.clone(),
            self.tool_socket(),
            self.directory.join("tasks"),
            role,
            Some(environment),
            timeout,
            command_prefix,
        )?);
        Ok(())
    }

    pub fn command(
        &self,
        role: &Role,
        environment: &[String],
        arguments: &[String],
    ) -> io::Result<(Job, std::process::Command)> {
        if self.lease.is_none() {
            return Err(io::Error::other("session is stopped"));
        }
        let environment = self.write_environment("harness.env", environment)?;
        super::stdio::command(
            &self.executable,
            &self.directory.join("tasks"),
            role,
            &environment,
            arguments,
        )
    }

    fn write_environment(&self, name: &str, environment: &[String]) -> io::Result<PathBuf> {
        if environment
            .iter()
            .any(|value| value.contains('\0') || !value.contains('='))
        {
            return Err(io::Error::other("invalid environment"));
        }
        let bytes = format!("{}\0", environment.join("\0"));
        let path = self.directory.join(name);
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)?;
        file.write_all(bytes.as_bytes())?;
        Ok(path)
    }

    pub fn run_harness(&self, role: &Role, arguments: &[String]) -> io::Result<(i32, Vec<u8>)> {
        if self.lease.is_none()
            || arguments.is_empty()
            || arguments.len() > 32
            || !arguments[0].starts_with('/')
            || arguments.iter().any(|argument| argument.contains('\0'))
        {
            return Err(io::Error::other("invalid native harness request"));
        }
        let body = format!("{}\0", arguments.join("\0"));
        if body.len() > MAX_REQUEST {
            return Err(io::Error::other("harness request too large"));
        }
        let (mut job, mut stream) = Job::start(
            &self.executable,
            &self.directory.join("tasks"),
            "__macos-exec-worker",
            &[
                role.profile.clone(),
                path_text(&role.workspace)?,
                path_text(&role.home)?,
                "30".into(),
            ],
            &[],
        )?;
        stream.set_nonblocking(false)?;
        stream.set_read_timeout(Some(Duration::from_secs(35)))?;
        stream.set_write_timeout(Some(Duration::from_secs(2)))?;
        stream.write_all(&(body.len() as u32).to_be_bytes())?;
        stream.write_all(body.as_bytes())?;
        let mut size = [0; 4];
        stream.read_exact(&mut size)?;
        let size = u32::from_be_bytes(size) as usize;
        if size == 0 || size > MAX_OUTPUT + 32 {
            return Err(io::Error::other("invalid harness response size"));
        }
        let mut bytes = vec![0; size];
        stream.read_exact(&mut bytes)?;
        let separator = bytes
            .iter()
            .position(|byte| *byte == b'\n')
            .ok_or_else(|| io::Error::other("invalid harness response"))?;
        let status = std::str::from_utf8(&bytes[..separator])
            .map_err(io::Error::other)?
            .parse()
            .map_err(io::Error::other)?;
        drop(stream);
        job.finish()?;
        Ok((status, bytes[separator + 1..].to_vec()))
    }

    pub fn revoke(&mut self) -> io::Result<()> {
        self.lease
            .as_mut()
            .ok_or_else(|| io::Error::other("session is stopped"))?
            .revoke()
    }

    pub fn finish(&mut self) -> io::Result<()> {
        if self.finished {
            return Ok(());
        }
        // Closing the controller stops forwarding but deliberately retains the leases.
        self.lease.take();
        self.tools.take();
        recover_contents(&self.directory)?;
        self.finished = true;
        Ok(())
    }

    pub fn recover(directory: &Path) -> io::Result<()> {
        private_directory(directory)?;
        let _lock = lock(directory, false)?;
        recover_contents(directory)
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if let Err(error) = self.finish() {
            eprintln!(
                "native session cleanup incomplete at {}: {error}",
                self.directory.display()
            );
        }
    }
}

fn recover_contents(directory: &Path) -> io::Result<()> {
    let marker = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(directory.join("owner"))?;
    let metadata = marker.metadata()?;
    if !metadata.is_file()
        || metadata.uid() != unsafe { libc::getuid() }
        || metadata.mode() & 0o077 != 0
        || metadata.len() > 128
    {
        return Err(io::Error::other("invalid native session record"));
    }
    let mut record = String::new();
    marker.take(129).read_to_string(&mut record)?;
    if record != format!("native-v1\n{}\n", boot_uuid()?) {
        return Err(io::Error::other(
            "invalid native session record or different boot",
        ));
    }
    // Any uncertain task ownership retains every address granted to that session.
    recover_jobs(&directory.join("tasks"))?;
    recover_jobs(&directory.join("leases"))?;
    fs::remove_dir_all(directory)
}

fn recover_jobs(directory: &Path) -> io::Result<()> {
    private_directory(directory)?;
    let mut failure = None;
    for entry in fs::read_dir(directory)? {
        if let Err(error) = Job::recover(&entry?.path()) {
            failure = Some(error);
        }
    }
    failure.map_or(Ok(()), Err)
}
