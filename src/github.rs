use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::backend::ProxyEndpoint;
use crate::fs_util::write_private;

pub(crate) fn prepare(session: &Path, endpoint: &ProxyEndpoint) -> Result<PathBuf> {
    let directory = session.join("github");
    fs::DirBuilder::new().mode(0o700).create(&directory)?;
    #[cfg(target_os = "macos")]
    let socket = endpoint.socket_dir.join("direct.sock");
    #[cfg(target_os = "linux")]
    let socket = {
        let _ = endpoint;
        PathBuf::from("/run/slopbox-host/authenticated-http/direct.sock")
    };
    let socket = socket.to_str().context("invalid GitHub socket path")?;
    // Unversioned config triggers gh's migration writer, even with no saved accounts.
    write_private(
        &directory.join("config.yml"),
        &format!(
            "version: 1\nhttp_unix_socket: {}\ngit_protocol: https\nprompt: disabled\n",
            serde_json::to_string(socket)?
        ),
    )?;
    write_private(&directory.join("hosts.yml"), "{}\n")?;
    Ok(directory)
}

pub(crate) fn environment(directory: &Path) -> Vec<(OsString, OsString)> {
    [
        ("GH_CONFIG_DIR", directory.as_os_str().to_owned()),
        ("GH_HOST", "github.com".into()),
        ("GH_TOKEN", "slopbox-brokered-authentication".into()),
        (
            "GH_ENTERPRISE_TOKEN",
            "slopbox-brokered-authentication".into(),
        ),
        ("GH_PROMPT_DISABLED", "1".into()),
        ("GH_NO_UPDATE_NOTIFIER", "1".into()),
        ("GH_NO_EXTENSION_UPDATE_NOTIFIER", "1".into()),
    ]
    .into_iter()
    .map(|(name, value)| (name.into(), value))
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    #[ignore = "requires GitHub CLI; set SLOPBOX_TEST_GH (no network or host credentials needed)"]
    fn gh_reads_generated_config_without_migration() {
        let gh = std::env::var_os("SLOPBOX_TEST_GH").expect("reviewed gh path required");
        let session = tempfile::tempdir().unwrap();
        let endpoint = ProxyEndpoint {
            socket_dir: "/private/account gateway".into(),
            port: 12345,
        };
        let directory = prepare(session.path(), &endpoint).unwrap();
        let config = fs::read(directory.join("config.yml")).unwrap();
        for name in ["config.yml", "hosts.yml"] {
            fs::set_permissions(directory.join(name), fs::Permissions::from_mode(0o400)).unwrap();
        }
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o500)).unwrap();
        let output = std::process::Command::new(gh)
            .args(["config", "get", "http_unix_socket"])
            .env_clear()
            .envs(environment(&directory))
            .env("HOME", session.path())
            .env("XDG_CONFIG_HOME", session.path())
            .current_dir(session.path())
            .stdin(std::process::Stdio::null())
            .output();
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
        let output = output.unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let expected = if cfg!(target_os = "macos") {
            "/private/account gateway/direct.sock\n"
        } else {
            "/run/slopbox-host/authenticated-http/direct.sock\n"
        };
        assert_eq!(String::from_utf8(output.stdout).unwrap(), expected);
        assert_eq!(fs::read(directory.join("config.yml")).unwrap(), config);
        assert_eq!(
            fs::read_to_string(directory.join("hosts.yml")).unwrap(),
            "{}\n"
        );
    }

    #[test]
    fn generated_config_contains_only_transport_settings_and_synthetic_auth() {
        let session = tempfile::tempdir().unwrap();
        let endpoint = ProxyEndpoint {
            socket_dir: "/private/account gateway".into(),
            port: 12345,
        };
        let directory = prepare(session.path(), &endpoint).unwrap();
        let config = fs::read_to_string(directory.join("config.yml")).unwrap();
        assert!(config.starts_with("version: 1\n"));
        assert!(config.contains("http_unix_socket: \""));
        assert!(config.contains("direct.sock\"\n"));
        assert!(config.contains("git_protocol: https\n"));
        assert!(!config.contains("oauth_token"));
        assert_eq!(
            fs::read_to_string(directory.join("hosts.yml")).unwrap(),
            "{}\n"
        );
        assert_eq!(
            fs::metadata(&directory).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(directory.join("config.yml"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        let environment = environment(&directory);
        assert!(
            environment
                .iter()
                .any(|(name, value)| name == "GH_TOKEN"
                    && value == "slopbox-brokered-authentication")
        );
        assert!(environment.iter().any(|(name, _)| name == "GH_CONFIG_DIR"));
    }
}
