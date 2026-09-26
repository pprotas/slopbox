use std::fs::File;
use std::io::{Read, Seek};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};

pub fn namespace_probe(bwrap: &Path, bash: &Path) -> Result<String> {
    let arguments = [
        "--die-with-parent",
        "--new-session",
        "--unshare-user",
        "--unshare-pid",
        "--unshare-ipc",
        "--unshare-uts",
        "--unshare-net",
        "--hostname",
        "slopbox-probe",
        "--cap-drop",
        "ALL",
        "--clearenv",
        "--ro-bind",
        "/nix/store",
        "/nix/store",
        "--proc",
        "/proc",
        "--dev",
        "/dev",
        "--tmpfs",
        "/tmp",
        "--chdir",
        "/",
        "--",
    ];
    let mut command = Command::new(bwrap);
    command
        .env_clear()
        .current_dir("/")
        .args(arguments)
        .arg(bwrap)
        .args(arguments)
        .arg(bash)
        .args(["--noprofile", "--norc", "-c", "exit 0"]);
    super::close_inherited_descriptors(&mut command);
    run_probe(&mut command, Duration::from_secs(5)).context(
        "native namespace probe failed; check host bubblewrap/user-namespace support and any enclosing container restrictions",
    )?;
    Ok("outer and nested tool namespaces work (no workspace or broker access)".into())
}

fn run_probe(command: &mut Command, timeout: Duration) -> Result<()> {
    // A file avoids pipe backpressure while we wait with a deadline.
    let mut stderr = tempfile::tempfile().context("failed to create probe diagnostic file")?;
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(stderr.try_clone()?)
        .spawn()
        .context("failed to start namespace probe")?;
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            result => {
                let _ = child.kill();
                let _ = child.wait();
                result.context("failed to wait for namespace probe")?;
                bail!("namespace probe timed out after {} ms", timeout.as_millis());
            }
        }
    };
    ensure!(status.success(), "{status}: {}", probe_error(&mut stderr)?);
    Ok(())
}

fn probe_error(stderr: &mut File) -> Result<String> {
    stderr.rewind()?;
    let mut bytes = Vec::new();
    stderr.take(8192).read_to_end(&mut bytes)?;
    Ok(String::from_utf8_lossy(&bytes).trim().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_probe_failure_without_pipe_backpressure() {
        let mut command = Command::new("bash");
        command
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap())
            .args([
                "--noprofile",
                "--norc",
                "-c",
                "printf 'probe failure\\n' >&2; printf '%s' {1..30000} >&2; exit 23",
            ]);
        let error = run_probe(&mut command, Duration::from_secs(5)).unwrap_err();
        let message = error.to_string();
        assert!(message.contains("23"));
        assert!(message.contains("probe failure"));
        assert!((8192..8300).contains(&message.len()));
    }

    #[test]
    fn stops_a_stuck_probe() {
        let mut command = Command::new("bash");
        command
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap())
            .args(["--noprofile", "--norc", "-c", "while :; do :; done"]);
        let error = run_probe(&mut command, Duration::from_millis(50)).unwrap_err();
        assert!(error.to_string().contains("timed out"));
    }

    #[test]
    fn accepts_a_successful_probe() {
        let mut command = Command::new("bash");
        command
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap())
            .args(["--noprofile", "--norc", "-c", "exit 0"]);
        run_probe(&mut command, Duration::from_secs(5)).unwrap();
    }
}
