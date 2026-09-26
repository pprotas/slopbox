use std::ffi::OsString;
use std::fs;
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};

pub const MOUNT: &str = "/run/slopbox-clipboard";
const MAX_IMAGE: usize = 20 * 1024 * 1024;
const MAX_SESSION: usize = 128 * 1024 * 1024;
const MAX_IMAGES: usize = 64;
const FORMATS: &[(&str, &str)] = &[
    ("image/png", "png"),
    ("image/jpeg", "jpg"),
    ("image/webp", "webp"),
    ("image/gif", "gif"),
];

pub struct Clipboard {
    directory: PathBuf,
    helper: Option<PathBuf>,
    display: Option<OsString>,
    runtime: Option<OsString>,
    bytes: usize,
    images: usize,
}

impl Clipboard {
    pub fn new(directory: PathBuf, helper: Option<PathBuf>) -> Result<Self> {
        fs::create_dir(&directory)?;
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))?;
        Ok(Self {
            directory,
            helper,
            display: std::env::var_os("WAYLAND_DISPLAY"),
            runtime: std::env::var_os("XDG_RUNTIME_DIR"),
            bytes: 0,
            images: 0,
        })
    }

    fn command(&self) -> Result<Command> {
        ensure!(
            cfg!(target_os = "linux"),
            "native macOS clipboard capture is not implemented; no pasteboard service is exposed"
        );
        let display = self
            .display
            .as_ref()
            .context("image paste requires a host Wayland session")?;
        let helper = self.helper.as_ref().context(
            "host wl-paste is missing; use packaged Slopbox or launch through nix develop",
        )?;
        let mut command = Command::new(helper);
        command
            .env_clear()
            .env("WAYLAND_DISPLAY", display)
            .env("LC_ALL", "C");
        if let Some(runtime) = &self.runtime {
            command.env("XDG_RUNTIME_DIR", runtime);
        }
        command
            .current_dir("/")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .process_group(0);
        #[cfg(target_os = "linux")]
        unsafe {
            command.pre_exec(|| {
                if libc::syscall(
                    libc::SYS_close_range,
                    3_u32,
                    u32::MAX,
                    libc::CLOSE_RANGE_CLOEXEC,
                ) == -1
                {
                    let error = io::Error::last_os_error();
                    if error.raw_os_error() != Some(libc::ENOSYS) {
                        return Err(error);
                    }
                }
                Ok(())
            });
        }
        Ok(command)
    }

    pub fn start(&self) -> Result<Capture> {
        ensure!(
            self.images < MAX_IMAGES && self.bytes < MAX_SESSION,
            "clipboard session limit reached (64 images or 128 MiB)"
        );
        let mut command = self.command()?;
        command.arg("--list-types");
        Ok(Capture {
            job: Job::start(command, 64 * 1024, Duration::from_secs(1))?,
            format: None,
        })
    }

    pub fn poll(&mut self, capture: &mut Capture) -> Result<Option<String>> {
        let Some(bytes) = capture.job.poll()? else {
            return Ok(None);
        };
        let Some((mime, extension)) = capture.format else {
            let types =
                std::str::from_utf8(&bytes).context("clipboard returned invalid MIME types")?;
            let format = FORMATS.iter().find(|(mime, _)| types.lines().any(|line| line.trim() == *mime))
                .copied().context("clipboard has no PNG, JPEG, WebP, or GIF image; use your terminal's paste shortcut for text")?;
            let mut command = self.command()?;
            command.args(["--type", format.0, "--no-newline"]);
            capture.job = Job::start(
                command,
                MAX_IMAGE.min(MAX_SESSION - self.bytes),
                Duration::from_secs(3),
            )?;
            capture.format = Some(format);
            return Ok(None);
        };
        ensure!(
            valid_header(mime, &bytes),
            "clipboard data does not match its image type"
        );
        ensure!(
            self.images < MAX_IMAGES && bytes.len() <= MAX_SESSION - self.bytes,
            "clipboard session limit reached"
        );
        // Publish atomically; no partially written file is visible in the guest mount.
        let mut file = tempfile::Builder::new()
            .prefix("image-")
            .suffix(&format!(".{extension}"))
            .tempfile_in(
                self.directory
                    .parent()
                    .context("clipboard directory has no parent")?,
            )?;
        file.write_all(&bytes)?;
        let name = file
            .path()
            .file_name()
            .context("clipboard image has no filename")?
            .to_string_lossy()
            .into_owned();
        file.persist_noclobber(self.directory.join(&name))?;
        self.bytes += bytes.len();
        self.images += 1;
        Ok(Some(format!("{MOUNT}/{name}")))
    }
}

fn valid_header(mime: &str, bytes: &[u8]) -> bool {
    match mime {
        "image/png" => bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
        "image/jpeg" => bytes.starts_with(b"\xff\xd8\xff"),
        "image/gif" => bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a"),
        "image/webp" => bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP"),
        _ => false,
    }
}

pub struct Capture {
    job: Job,
    format: Option<(&'static str, &'static str)>,
}
impl Capture {
    pub fn descriptor(&self) -> i32 {
        if self.job.eof {
            -1
        } else {
            self.job.stdout.as_raw_fd()
        }
    }
}

struct Job {
    child: Child,
    stdout: ChildStdout,
    deadline: Instant,
    limit: usize,
    bytes: Vec<u8>,
    eof: bool,
    reaped: bool,
}
impl Job {
    fn start(mut command: Command, limit: usize, timeout: Duration) -> Result<Self> {
        let mut child = command.spawn().context("failed to start host wl-paste")?;
        let stdout = child
            .stdout
            .take()
            .context("clipboard helper has no stdout")?;
        let job = Self {
            child,
            stdout,
            deadline: Instant::now() + timeout,
            limit,
            bytes: Vec::new(),
            eof: false,
            reaped: false,
        };
        let result =
            unsafe { libc::fcntl(job.stdout.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) };
        if result == -1 {
            return Err(io::Error::last_os_error().into());
        }
        Ok(job)
    }

    fn poll(&mut self) -> Result<Option<Vec<u8>>> {
        ensure!(
            Instant::now() < self.deadline,
            "host clipboard read timed out"
        );
        let mut buffer = [0u8; 8192];
        for _ in 0..8 {
            match self.stdout.read(&mut buffer) {
                Ok(0) => {
                    self.eof = true;
                    break;
                }
                Ok(count) => {
                    ensure!(
                        count <= self.limit.saturating_sub(self.bytes.len()),
                        "clipboard data exceeds the size limit (20 MiB per image, 128 MiB per session)"
                    );
                    self.bytes.extend_from_slice(&buffer[..count]);
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e.into()),
            }
        }
        if self.eof
            && let Some(status) = self.child.try_wait()?
        {
            self.reaped = true;
            if !status.success() {
                bail!("host wl-paste failed; check the clipboard and Wayland session ({status})");
            }
            return Ok(Some(std::mem::take(&mut self.bytes)));
        }
        Ok(None)
    }
}
impl Drop for Job {
    fn drop(&mut self) {
        if !self.reaped {
            unsafe {
                libc::kill(-(self.child.id() as i32), libc::SIGKILL);
            }
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(target_os = "linux")]
    use std::os::unix::fs::PermissionsExt;
    #[cfg(target_os = "linux")]
    use std::path::Path;

    #[test]
    fn accepts_only_supported_image_headers() {
        for (mime, bytes) in [
            ("image/png", b"\x89PNG\r\n\x1a\n".as_slice()),
            ("image/jpeg", b"\xff\xd8\xff"),
            ("image/gif", b"GIF89a"),
            ("image/webp", b"RIFF1234WEBP"),
        ] {
            assert!(valid_header(mime, bytes));
            assert!(!valid_header(mime, b"<html>"));
        }
        assert!(!valid_header("image/svg+xml", b"<svg/>"));
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn imports_only_on_request_and_publishes_private_distinct_files() {
        let root = tempfile::tempdir().unwrap();
        let helper = root.path().join("wl-paste");
        fs::write(&helper, b"#!/bin/sh\nif [ \"$1\" = --list-types ]; then printf 'text/plain\\nimage/png\\n'; else printf '\\211PNG\\r\\n\\032\\n'; fi\n").unwrap();
        fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).unwrap();
        let directory = root.path().join("clipboard");
        let mut clipboard = Clipboard::new(directory.clone(), Some(helper)).unwrap();
        clipboard.display = Some("test-wayland".into());
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 0);
        for expected in 1..=2 {
            let mut capture = clipboard.start().unwrap();
            let path = loop {
                if let Some(path) = clipboard.poll(&mut capture).unwrap() {
                    break path;
                }
                std::thread::sleep(Duration::from_millis(2));
            };
            let name = Path::new(&path).file_name().unwrap();
            assert_eq!(
                fs::read(directory.join(name)).unwrap(),
                b"\x89PNG\r\n\x1a\n"
            );
            assert_eq!(
                fs::metadata(directory.join(name))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
            assert_eq!(fs::read_dir(&directory).unwrap().count(), expected);
        }
        clipboard.images = MAX_IMAGES;
        assert!(clipboard.start().is_err());
        clipboard.images = 0;
        clipboard.bytes = MAX_SESSION;
        assert!(clipboard.start().is_err());
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn unsupported_or_mislabeled_clipboards_publish_nothing() {
        for script in [
            "printf 'text/plain\\n'",
            "if [ \"$1\" = --list-types ]; then printf 'image/png\\n'; else printf 'not an image'; fi",
        ] {
            let root = tempfile::tempdir().unwrap();
            let helper = root.path().join("wl-paste");
            fs::write(&helper, format!("#!/bin/sh\n{script}\n")).unwrap();
            fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).unwrap();
            let directory = root.path().join("clipboard");
            let mut clipboard = Clipboard::new(directory.clone(), Some(helper)).unwrap();
            clipboard.display = Some("test-wayland".into());
            let mut capture = clipboard.start().unwrap();
            loop {
                match clipboard.poll(&mut capture) {
                    Err(_) => break,
                    Ok(None) => std::thread::sleep(Duration::from_millis(2)),
                    Ok(Some(_)) => panic!("invalid image was published"),
                }
            }
            assert_eq!(fs::read_dir(&directory).unwrap().count(), 0);
            assert_eq!(clipboard.bytes, 0);
        }
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn cancelling_capture_kills_the_helper_without_publishing() {
        let root = tempfile::tempdir().unwrap();
        let helper = root.path().join("wl-paste");
        fs::write(&helper, "#!/bin/sh\nwhile :; do :; done\n").unwrap();
        fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).unwrap();
        let directory = root.path().join("clipboard");
        let mut clipboard = Clipboard::new(directory.clone(), Some(helper)).unwrap();
        clipboard.display = Some("test-wayland".into());
        let capture = clipboard.start().unwrap();
        let pid = capture.job.child.id();
        drop(capture);
        assert_eq!(unsafe { libc::kill(pid as i32, 0) }, -1);
        assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::ESRCH));
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 0);
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn macos_clipboard_capture_is_explicitly_unsupported() {
        let root = tempfile::tempdir().unwrap();
        let clipboard =
            Clipboard::new(root.path().join("clipboard"), Some("/not-executed".into())).unwrap();
        let error = clipboard.command().unwrap_err();
        assert!(
            error
                .to_string()
                .contains("native macOS clipboard capture is not implemented")
        );
    }

    #[test]
    fn failed_oversized_and_stuck_helpers_are_bounded_and_reaped() {
        for (script, limit, timeout) in [
            ("printf too-large", 2, Duration::from_secs(1)),
            ("exit 1", 10, Duration::from_secs(1)),
            ("while :; do :; done", 10, Duration::from_millis(20)),
        ] {
            let mut command = Command::new("/bin/sh");
            command
                .args(["-c", script])
                .stdout(Stdio::piped())
                .process_group(0);
            let mut job = Job::start(command, limit, timeout).unwrap();
            let pid = job.child.id();
            loop {
                match job.poll() {
                    Err(_) => break,
                    Ok(None) => std::thread::sleep(Duration::from_millis(2)),
                    Ok(Some(_)) => panic!("bad helper succeeded"),
                }
            }
            drop(job);
            assert_eq!(unsafe { libc::kill(pid as i32, 0) }, -1);
            assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::ESRCH));
        }
    }
}
