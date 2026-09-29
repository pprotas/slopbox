use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use super::job::{Job, boot_uuid, lock, private_directory};
use super::relay::{Lease, Ports, Routes};

pub struct Role {
    pub profile: String,
    pub workspace: PathBuf,
    pub home: PathBuf,
}

pub struct Session {
    directory: PathBuf,
    executable: PathBuf,
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

    pub fn command(
        &self,
        role: &Role,
        environment: &[String],
        arguments: &[String],
    ) -> io::Result<(Job, std::process::Command)> {
        if self.lease.is_none() {
            return Err(io::Error::other("session is stopped"));
        }
        let environment = self.write_environment("command.env", environment)?;
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

    pub fn finish(&mut self) -> io::Result<()> {
        if self.finished {
            return Ok(());
        }
        // Closing the controller stops forwarding but deliberately retains the leases.
        self.lease.take();
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
