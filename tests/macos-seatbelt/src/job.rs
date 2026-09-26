use std::io::{self, Read};
use std::net::SocketAddr;
use std::ops::{Deref, DerefMut};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use crate::native_job;

pub struct Job {
    native: native_job::Job,
    endpoint: Option<SocketAddr>,
}

impl Job {
    pub fn start(
        executable: &Path,
        control: &Path,
        profile: &str,
        workspace: &Path,
        home: &Path,
    ) -> io::Result<(Self, UnixStream)> {
        Self::launch(executable, control, profile, workspace, home, false)
    }

    pub fn start_with_endpoint(
        executable: &Path,
        control: &Path,
        profile: &str,
        workspace: &Path,
        home: &Path,
    ) -> io::Result<(Self, UnixStream)> {
        Self::launch(executable, control, profile, workspace, home, true)
    }

    fn launch(
        executable: &Path,
        control: &Path,
        profile: &str,
        workspace: &Path,
        home: &Path,
        endpoint: bool,
    ) -> io::Result<(Self, UnixStream)> {
        let arguments = [
            profile.to_owned(),
            workspace.to_str().unwrap().into(),
            home.to_str().unwrap().into(),
            "2".into(),
        ];
        let (native, mut stream) = native_job::Job::start(
            executable,
            control,
            if endpoint {
                "leased-worker"
            } else {
                "__macos-exec-worker"
            },
            &arguments,
            if endpoint { &["Broker"] } else { &[] },
        )?;
        let mut job = Self {
            native,
            endpoint: None,
        };
        if endpoint {
            stream.set_nonblocking(false)?;
            stream.set_read_timeout(Some(Duration::from_secs(2)))?;
            let mut port = [0; 2];
            stream.read_exact(&mut port)?;
            let port = u16::from_be_bytes(port);
            if port == 0 {
                return Err(io::Error::other("invalid broker port"));
            }
            job.endpoint = Some(([127, 0, 0, 1], port).into());
            stream.set_read_timeout(None)?;
            stream.set_nonblocking(true)?;
        }
        Ok((job, stream))
    }

    pub fn endpoint(&self) -> Option<SocketAddr> {
        self.endpoint
    }

    pub fn recover(directory: &Path) -> io::Result<()> {
        native_job::Job::recover(directory)
    }
}

impl Deref for Job {
    type Target = native_job::Job;
    fn deref(&self) -> &Self::Target {
        &self.native
    }
}

impl DerefMut for Job {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.native
    }
}
