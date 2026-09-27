use std::env;
use std::fs;
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, ensure};

use crate::command::trusted_executable;

pub fn from_environment(variable: &str) -> Result<String> {
    ensure!(
        !variable.is_empty()
            && variable
                .bytes()
                .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_'),
        "invalid secret environment variable name"
    );
    let secret = env::var(variable).with_context(|| format!("{variable} is not set"))?;
    validate_secret(secret)
}

pub fn from_command(
    arguments: &[String],
    #[cfg(target_os = "macos")] workspace: &Path,
) -> Result<String> {
    let program = arguments.first().context("secret command is empty")?;
    ensure!(
        !program.is_empty()
            && program
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
            && arguments.iter().all(|argument| !argument.contains('\0')),
        "secret command requires a trusted helper name and valid arguments"
    );
    let executable = trusted_executable(
        program,
        #[cfg(target_os = "macos")]
        workspace,
    )?;
    let mut command = Command::new(executable);
    command.args(&arguments[1..]);
    command_output(&mut command)
}

fn command_output(command: &mut Command) -> Result<String> {
    let program = Path::new(command.get_program());
    let path = env::join_paths([
        program
            .parent()
            .context("secret command has no parent directory")?,
        Path::new("/usr/bin"),
        Path::new("/bin"),
    ])?;
    command
        .env_clear()
        .current_dir("/")
        .env("PATH", path)
        .env("GH_NO_UPDATE_NOTIFIER", "1")
        .env("GH_NO_EXTENSION_UPDATE_NOTIFIER", "1")
        .env("GH_PROMPT_DISABLED", "1")
        .stdin(Stdio::null())
        .stderr(Stdio::null());
    for name in [
        "HOME",
        "XDG_CONFIG_HOME",
        "GH_CONFIG_DIR",
        "DBUS_SESSION_BUS_ADDRESS",
    ] {
        if let Some(value) = env::var_os(name) {
            command.env(name, value);
        }
    }
    let mut child = command
        .stdout(Stdio::piped())
        .spawn()
        .context("failed to execute secret command")?;
    let mut output = Vec::new();
    let read = child
        .stdout
        .take()
        .expect("secret stdout is piped")
        .take(64 * 1024 + 1)
        .read_to_end(&mut output);
    if read.is_err() || output.len() > 64 * 1024 {
        let _ = child.kill();
        let _ = child.wait();
        read.context("failed to read secret command output")?;
        anyhow::bail!("secret command output is unexpectedly large");
    }
    ensure!(
        child
            .wait()
            .context("failed to wait for secret command")?
            .success(),
        "secret command failed"
    );
    validate_secret(String::from_utf8(output).context("secret command output is not UTF-8")?)
}

pub fn from_sops(
    path: &Path,
    key: &str,
    #[cfg(target_os = "macos")] workspace: &Path,
) -> Result<String> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("failed to inspect SOPS file {}", path.display()))?;
    ensure!(
        metadata.is_file() && !metadata.file_type().is_symlink(),
        "SOPS source must be a regular file"
    );
    ensure!(
        key.starts_with("[\"") && key.ends_with("\"]") && !key.contains('\n'),
        "SOPS key must be a top-level JSON path"
    );

    let sops = trusted_executable(
        "sops",
        #[cfg(target_os = "macos")]
        workspace,
    )?;
    let mut command = Command::new(sops);
    command
        .env_clear()
        .arg("--decrypt")
        .arg("--extract")
        .arg(key)
        .arg(path);
    for name in ["HOME", "XDG_CONFIG_HOME", "SOPS_AGE_KEY_FILE"] {
        if let Some(value) = env::var_os(name) {
            command.env(name, value);
        }
    }
    let output = command.output().context("failed to execute sops")?;
    ensure!(output.status.success(), "sops secret decryption failed");
    ensure!(
        output.stdout.len() <= 64 * 1024,
        "decrypted secret is unexpectedly large"
    );
    validate_secret(String::from_utf8(output.stdout).context("decrypted secret is not UTF-8")?)
}

fn validate_secret(secret: String) -> Result<String> {
    let secret = secret.trim().to_owned();
    ensure!(!secret.is_empty(), "secret is empty");
    ensure!(
        !secret.chars().any(char::is_whitespace),
        "secret contains whitespace"
    );
    Ok(secret)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_environment_variable_names() {
        assert!(from_environment("not-valid").is_err());
    }

    #[test]
    fn command_secrets_clear_ambient_state_and_do_not_evaluate_arguments() {
        let bash = crate::command::find_optional_executable("bash", &env::var_os("PATH").unwrap())
            .expect("bash test dependency required");
        let mut command = Command::new(bash);
        command
            .env("GH_TOKEN", "ambient-token")
            .env("BASH_ENV", "/not-a-startup-file")
            .args([
                "--noprofile", "--norc", "-c",
                "test \"$PWD\" = / && test -z \"${GH_TOKEN+x}\" && test -z \"${BASH_ENV+x}\" && test \"$GH_PROMPT_DISABLED\" = 1 && printf '%s' \"$1\"",
                "fixture", "$(false)",
            ]);
        assert_eq!(command_output(&mut command).unwrap(), "$(false)");
    }

    #[test]
    fn command_secret_errors_do_not_echo_output() {
        let bash = crate::command::find_optional_executable("bash", &env::var_os("PATH").unwrap())
            .expect("bash test dependency required");
        for (script, expected) in [
            (
                "printf fixture-secret; printf fixture-secret >&2; exit 1",
                "secret command failed",
            ),
            (
                "printf '%65537s' x",
                "secret command output is unexpectedly large",
            ),
            ("printf ' '", "secret is empty"),
            ("printf '\\377'", "secret command output is not UTF-8"),
        ] {
            let mut command = Command::new(&bash);
            command.args(["--noprofile", "--norc", "-c", script]);
            let error = command_output(&mut command).unwrap_err().to_string();
            assert_eq!(error, expected);
        }
    }

    #[test]
    fn command_secret_executables_cannot_be_workspace_paths() {
        for arguments in [
            vec![],
            vec!["./gh".into()],
            vec!["gh auth token".into()],
            vec!["gh".into(), "bad\0argument".into()],
        ] {
            assert!(
                from_command(
                    &arguments,
                    #[cfg(target_os = "macos")]
                    Path::new("/"),
                )
                .is_err()
            );
        }
    }

    #[test]
    fn rejects_empty_secrets() {
        assert!(validate_secret(" \n".to_owned()).is_err());
    }
}
