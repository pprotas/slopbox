use std::env;
use std::ffi::{OsStr, OsString};
use std::fs::{self, DirBuilder, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};

const MAX_SIGNING_PAYLOAD: usize = 16 * 1024 * 1024;
const MAX_SIGNATURE: usize = 64 * 1024;
#[cfg(target_os = "linux")]
static SESSION_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Clone)]
pub struct GitSigningIdentity {
    pub name: String,
    pub email: String,
    pub fingerprint: String,
}

struct PreparedSigner {
    identity: GitSigningIdentity,
    public_key: String,
    agent_socket: PathBuf,
    ssh_keygen: PathBuf,
}

pub struct GitSigningSession {
    runtime_dir: PathBuf,
    socket_path: PathBuf,
    identity: GitSigningIdentity,
    public_key: String,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl GitSigningSession {
    pub fn start(identity: GitSigningIdentity) -> Result<Self> {
        validate_identity(&identity)?;
        let signer = prepare_signer(identity)?;
        #[cfg(target_os = "linux")]
        let runtime_dir = {
            let runtime_base = env::var_os("XDG_RUNTIME_DIR")
                .map(PathBuf::from)
                .context("XDG_RUNTIME_DIR is required for Git signing")?
                .join("slopbox");
            secure_dir(&runtime_base)?;
            let directory = runtime_base.join(format!(
                "git-{}-{}",
                std::process::id(),
                SESSION_ID.fetch_add(1, Ordering::Relaxed)
            ));
            secure_dir(&directory)?;
            directory
        };
        #[cfg(target_os = "macos")]
        let directory = tempfile::Builder::new()
            .prefix("git-")
            .tempdir_in(crate::backend::macos::runtime_root()?)?;
        #[cfg(target_os = "macos")]
        let runtime_dir = directory.path().to_path_buf();
        let socket_path = runtime_dir.join("gateway.sock");
        let listener = UnixListener::bind(&socket_path)
            .with_context(|| format!("failed to bind {}", socket_path.display()))?;
        fs::set_permissions(&socket_path, fs::Permissions::from_mode(0o600))?;
        listener.set_nonblocking(true)?;

        let identity = signer.identity.clone();
        let public_key = signer.public_key.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let thread_runtime = runtime_dir.clone();
        let signer = Arc::new(signer);
        let thread = thread::Builder::new()
            .name("slopbox-git-signing".into())
            .spawn(move || signing_loop(listener, signer, thread_runtime, thread_stop))
            .context("failed to start Git signing broker")?;

        #[cfg(target_os = "macos")]
        let runtime_dir = directory.keep();
        Ok(Self {
            runtime_dir,
            socket_path,
            identity,
            public_key,
            stop,
            thread: Some(thread),
        })
    }

    pub fn socket_dir(&self) -> &Path {
        &self.runtime_dir
    }

    pub fn identity(&self) -> &GitSigningIdentity {
        &self.identity
    }

    pub fn public_key(&self) -> &str {
        &self.public_key
    }
}

impl Drop for GitSigningSession {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = UnixStream::connect(&self.socket_path);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        let _ = fs::remove_dir_all(&self.runtime_dir);
    }
}

pub fn run_helper(arguments: &[OsString], socket: &Path) -> Result<()> {
    let payload_path = parse_helper_arguments(arguments)?;
    let payload = fs::read(&payload_path).with_context(|| {
        format!(
            "failed to read Git signing payload {}",
            payload_path.display()
        )
    })?;
    ensure!(
        payload.len() <= MAX_SIGNING_PAYLOAD,
        "Git signing payload is too large"
    );

    let mut stream = UnixStream::connect(socket).context("Git signing broker is unavailable")?;
    stream.write_all(&(payload.len() as u64).to_be_bytes())?;
    stream.write_all(&payload)?;
    stream.shutdown(std::net::Shutdown::Write)?;

    let mut status = [0_u8; 1];
    stream.read_exact(&mut status)?;
    ensure!(status[0] == 0, "Git signing broker refused the request");
    let mut length = [0_u8; 8];
    stream.read_exact(&mut length)?;
    let length = u64::from_be_bytes(length) as usize;
    ensure!(
        length <= MAX_SIGNATURE,
        "Git signature is unexpectedly large"
    );
    let mut signature = vec![0_u8; length];
    stream.read_exact(&mut signature)?;

    let mut signature_path = payload_path.as_os_str().to_os_string();
    signature_path.push(".sig");
    let signature_path = PathBuf::from(signature_path);
    let mut output = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&signature_path)
        .with_context(|| {
            format!(
                "failed to create Git signature {}",
                signature_path.display()
            )
        })?;
    output.write_all(&signature)?;
    Ok(())
}

fn parse_helper_arguments(arguments: &[OsString]) -> Result<PathBuf> {
    ensure!(
        !arguments.is_empty(),
        "Git signing helper requires arguments"
    );
    let mut index = 0;
    let mut operation = None;
    let mut namespace = None;
    let mut public_key = None;
    let mut use_agent = false;
    let mut payload = None;
    while index < arguments.len() {
        match arguments[index].as_os_str() {
            value if value == OsStr::new("-Y") => {
                index += 1;
                operation = arguments.get(index).cloned();
            }
            value if value == OsStr::new("-n") => {
                index += 1;
                namespace = arguments.get(index).cloned();
            }
            value if value == OsStr::new("-f") => {
                index += 1;
                public_key = arguments.get(index).cloned();
            }
            value if value == OsStr::new("-U") => use_agent = true,
            value if value.to_string_lossy().starts_with('-') => {
                bail!("unsupported Git signing helper argument")
            }
            value => {
                ensure!(payload.is_none(), "multiple Git signing payloads provided");
                payload = Some(PathBuf::from(value));
            }
        }
        index += 1;
    }
    ensure!(
        operation.as_deref() == Some(OsStr::new("sign"))
            && namespace.as_deref() == Some(OsStr::new("git"))
            && public_key.is_some()
            && use_agent,
        "unsupported Git signing operation"
    );
    payload.context("Git signing payload is missing")
}

fn prepare_signer(identity: GitSigningIdentity) -> Result<PreparedSigner> {
    let agent_socket =
        PathBuf::from(env::var_os("SSH_AUTH_SOCK").context("host SSH_AUTH_SOCK is not set")?);
    let metadata = fs::metadata(&agent_socket).with_context(|| {
        format!(
            "failed to inspect SSH agent socket {}",
            agent_socket.display()
        )
    })?;
    ensure!(
        metadata.file_type().is_socket(),
        "host SSH_AUTH_SOCK is not a Unix socket"
    );
    #[cfg(target_os = "linux")]
    let (ssh_add, ssh_keygen) = (
        crate::command::trusted_executable("ssh-add")?,
        crate::command::trusted_executable("ssh-keygen")?,
    );
    #[cfg(target_os = "macos")]
    let (ssh_add, ssh_keygen) = (
        PathBuf::from("/usr/bin/ssh-add"),
        PathBuf::from("/usr/bin/ssh-keygen"),
    );
    let output = Command::new(ssh_add)
        .env_clear()
        .env("SSH_AUTH_SOCK", &agent_socket)
        .arg("-L")
        .output()
        .context("failed to list host SSH agent keys")?;
    ensure!(
        output.status.success(),
        "host SSH agent has no available keys"
    );
    ensure!(
        output.stdout.len() <= 1024 * 1024,
        "host SSH agent returned too many keys"
    );
    let keys = String::from_utf8(output.stdout).context("host SSH agent returned invalid keys")?;
    let mut matching = Vec::new();
    for key in keys.lines().filter(|line| !line.trim().is_empty()) {
        if key_fingerprint(&ssh_keygen, key)? == identity.fingerprint {
            matching.push(key.to_owned());
        }
    }
    ensure!(
        matching.len() == 1,
        "expected exactly one host SSH agent key matching {}",
        identity.fingerprint
    );
    Ok(PreparedSigner {
        identity,
        public_key: matching.remove(0),
        agent_socket,
        ssh_keygen,
    })
}

fn key_fingerprint(ssh_keygen: &Path, public_key: &str) -> Result<String> {
    let mut child = Command::new(ssh_keygen)
        .env_clear()
        .args(["-E", "sha256", "-lf", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("failed to inspect SSH public key")?;
    child
        .stdin
        .take()
        .context("ssh-keygen stdin is unavailable")?
        .write_all(public_key.as_bytes())?;
    let output = child.wait_with_output()?;
    ensure!(
        output.status.success(),
        "host SSH agent returned an invalid key"
    );
    let output = String::from_utf8(output.stdout)?;
    output
        .split_whitespace()
        .nth(1)
        .map(str::to_owned)
        .context("ssh-keygen did not return a key fingerprint")
}

fn signing_loop(
    listener: UnixListener,
    signer: Arc<PreparedSigner>,
    runtime_dir: PathBuf,
    stop: Arc<AtomicBool>,
) {
    let counter = Arc::new(AtomicU64::new(0));
    while !stop.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, _)) => {
                let signer = Arc::clone(&signer);
                let runtime_dir = runtime_dir.clone();
                let counter = Arc::clone(&counter);
                thread::spawn(move || {
                    if let Err(error) =
                        handle_signing_request(stream, &signer, &runtime_dir, &counter)
                    {
                        eprintln!("slopbox Git signing broker: {error:#}");
                    }
                });
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(20));
            }
            Err(error) => {
                eprintln!("slopbox Git signing broker: accept failed: {error}");
                thread::sleep(Duration::from_millis(20));
            }
        }
    }
}

fn handle_signing_request(
    mut stream: UnixStream,
    signer: &PreparedSigner,
    runtime_dir: &Path,
    counter: &AtomicU64,
) -> Result<()> {
    // Darwin accepts inherit the listener's nonblocking flag.
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(Duration::from_secs(30)))?;
    let result = sign_request(&mut stream, signer, runtime_dir, counter);
    match result {
        Ok(signature) => {
            stream.write_all(&[0])?;
            stream.write_all(&(signature.len() as u64).to_be_bytes())?;
            stream.write_all(&signature)?;
            Ok(())
        }
        Err(error) => {
            let _ = stream.write_all(&[1]);
            Err(error)
        }
    }
}

fn sign_request(
    stream: &mut UnixStream,
    signer: &PreparedSigner,
    runtime_dir: &Path,
    counter: &AtomicU64,
) -> Result<Vec<u8>> {
    let mut length = [0_u8; 8];
    stream.read_exact(&mut length)?;
    let length = u64::from_be_bytes(length) as usize;
    ensure!(
        length <= MAX_SIGNING_PAYLOAD,
        "Git signing payload is too large"
    );
    let mut payload = vec![0_u8; length];
    stream.read_exact(&mut payload)?;
    validate_commit_payload(&payload, &signer.identity)?;

    let id = counter.fetch_add(1, Ordering::Relaxed);
    let request_dir = runtime_dir.join(format!("request-{}-{id}", std::process::id()));
    secure_dir(&request_dir)?;
    let result = (|| {
        let key_path = request_dir.join("key.pub");
        let payload_path = request_dir.join("payload");
        write_private(&key_path, signer.public_key.as_bytes())?;
        write_private(&payload_path, &payload)?;
        let output = Command::new(&signer.ssh_keygen)
            .env_clear()
            .env("SSH_AUTH_SOCK", &signer.agent_socket)
            .args(["-Y", "sign", "-n", "git", "-f"])
            .arg(&key_path)
            .arg("-U")
            .arg(&payload_path)
            .output()
            .context("failed to execute host SSH signer")?;
        ensure!(
            output.status.success(),
            "host SSH agent refused Git signing"
        );
        let mut signature_path = payload_path.as_os_str().to_os_string();
        signature_path.push(".sig");
        let signature = fs::read(PathBuf::from(signature_path))?;
        ensure!(
            signature.len() <= MAX_SIGNATURE,
            "Git signature is too large"
        );
        Ok(signature)
    })();
    let _ = fs::remove_dir_all(&request_dir);
    result
}

fn validate_commit_payload(payload: &[u8], identity: &GitSigningIdentity) -> Result<()> {
    ensure!(
        !payload.contains(&0),
        "Git signing payload contains a NUL byte"
    );
    let header_end = payload
        .windows(2)
        .position(|window| window == b"\n\n")
        .context("Git signing payload has no commit header")?;
    let header =
        std::str::from_utf8(&payload[..header_end]).context("Git commit header is not UTF-8")?;
    ensure!(
        header
            .lines()
            .next()
            .is_some_and(|line| line.starts_with("tree ")),
        "only Git commit objects may be signed"
    );
    let expected_author = format!("author {} <{}> ", identity.name, identity.email);
    let expected_committer = format!("committer {} <{}> ", identity.name, identity.email);
    let mut authors = header.lines().filter(|line| line.starts_with("author "));
    ensure!(
        authors
            .next()
            .is_some_and(|line| line.starts_with(&expected_author))
            && authors.next().is_none(),
        "Git commit author does not match the configured identity"
    );
    let mut committers = header.lines().filter(|line| line.starts_with("committer "));
    ensure!(
        committers
            .next()
            .is_some_and(|line| line.starts_with(&expected_committer))
            && committers.next().is_none(),
        "Git commit committer does not match the configured identity"
    );
    Ok(())
}

fn validate_identity(identity: &GitSigningIdentity) -> Result<()> {
    ensure!(
        !identity.name.trim().is_empty()
            && !identity.name.contains(['\r', '\n', '<', '>'])
            && !identity.email.trim().is_empty()
            && !identity.email.contains(['\r', '\n', '<', '>', ' ']),
        "Git signing identity is invalid"
    );
    ensure!(
        identity.fingerprint.starts_with("SHA256:")
            && identity
                .fingerprint
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric()
                    || matches!(byte, b'+' | b'/' | b'=' | b':')),
        "Git signing fingerprint is invalid"
    );
    Ok(())
}

fn secure_dir(path: &Path) -> Result<()> {
    let mut builder = DirBuilder::new();
    builder.recursive(true).mode(0o700);
    builder.create(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

fn write_private(path: &Path, value: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(value)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity() -> GitSigningIdentity {
        GitSigningIdentity {
            name: "pi".to_owned(),
            email: "pi@pawelprotas.com".to_owned(),
            fingerprint: "SHA256:abc".to_owned(),
        }
    }

    #[test]
    fn accepts_only_commits_for_the_configured_identity() {
        let valid = b"tree abc\nauthor pi <pi@pawelprotas.com> 1 +0000\ncommitter pi <pi@pawelprotas.com> 1 +0000\n\nmessage\n";
        assert!(validate_commit_payload(valid, &identity()).is_ok());
        let wrong = b"tree abc\nauthor other <other@example.com> 1 +0000\ncommitter pi <pi@pawelprotas.com> 1 +0000\n\nmessage\n";
        assert!(validate_commit_payload(wrong, &identity()).is_err());
        let duplicate_author = b"tree abc\nauthor pi <pi@pawelprotas.com> 1 +0000\nauthor other <other@example.com> 1 +0000\ncommitter pi <pi@pawelprotas.com> 1 +0000\n\nmessage\n";
        assert!(validate_commit_payload(duplicate_author, &identity()).is_err());
        let duplicate_committer = b"tree abc\nauthor pi <pi@pawelprotas.com> 1 +0000\ncommitter other <other@example.com> 1 +0000\ncommitter pi <pi@pawelprotas.com> 1 +0000\n\nmessage\n";
        assert!(validate_commit_payload(duplicate_committer, &identity()).is_err());
        assert!(validate_commit_payload(b"arbitrary", &identity()).is_err());
    }

    #[test]
    fn signing_requests_reset_inherited_nonblocking_mode() {
        use std::os::fd::AsRawFd;

        let (mut client, stream) = UnixStream::pair().unwrap();
        stream.set_nonblocking(true).unwrap();
        let observer = stream.try_clone().unwrap();
        let signer = PreparedSigner {
            identity: identity(),
            public_key: "unused".into(),
            agent_socket: PathBuf::from("unused"),
            ssh_keygen: PathBuf::from("unused"),
        };
        let payload = b"not a commit";
        client
            .write_all(&(payload.len() as u64).to_be_bytes())
            .unwrap();
        client.write_all(payload).unwrap();
        let error =
            handle_signing_request(stream, &signer, Path::new("unused"), &AtomicU64::new(0))
                .unwrap_err();
        assert_eq!(
            error.to_string(),
            "Git signing payload has no commit header"
        );
        let mut status = [0];
        client.read_exact(&mut status).unwrap();
        assert_eq!(status, [1]);
        let flags = unsafe { libc::fcntl(observer.as_raw_fd(), libc::F_GETFL) };
        assert!(flags >= 0);
        assert_eq!(flags & libc::O_NONBLOCK, 0);
    }

    #[test]
    fn parses_git_ssh_signing_arguments() {
        let arguments = [
            "-Y",
            "sign",
            "-n",
            "git",
            "-f",
            "/tmp/key",
            "-U",
            "/tmp/data",
        ]
        .map(OsString::from);
        assert_eq!(
            parse_helper_arguments(&arguments).unwrap(),
            PathBuf::from("/tmp/data")
        );
    }
}
