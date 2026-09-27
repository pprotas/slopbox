use std::env;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};

use anyhow::{Context, Result, bail, ensure};

use crate::ToolNetwork;

const GENERAL_GATEWAY_SOCKET: &str = "/run/slopbox-host/general/gateway.sock";
const AUTHENTICATED_HTTP_GATEWAY_SOCKET: &str = "/run/slopbox-host/authenticated-http/gateway.sock";

pub fn run(network: ToolNetwork, child: &[OsString]) -> Result<ExitStatus> {
    ensure!(!child.is_empty(), "a tool command is required");
    ensure!(
        env::var_os("SLOPBOX_SANDBOX").as_deref() == Some(OsStr::new("1")),
        "slopbox tool-run must be invoked inside a Slopbox sandbox"
    );

    let workspace = env::var_os("SLOPBOX_WORKSPACE")
        .map(PathBuf::from)
        .context("SLOPBOX_WORKSPACE is not set")?;
    let workspace = fs::canonicalize(&workspace)
        .with_context(|| format!("failed to resolve workspace {}", workspace.display()))?;
    let current_dir = fs::canonicalize(env::current_dir()?)
        .context("failed to resolve the tool working directory")?;
    ensure!(
        current_dir.starts_with(&workspace),
        "tool working directory is outside the workspace"
    );

    let tool_home = Path::new("/run/slopbox-tool-home");
    ensure!(tool_home.is_dir(), "Slopbox tool home is unavailable");

    let path = env::var_os("PATH").context("PATH is not set")?;
    let bwrap = find_executable("bwrap", &path)?;
    let mut command = Command::new(bwrap);
    command.env_clear();
    command.args([
        "--die-with-parent",
        "--new-session",
        "--unshare-user",
        "--unshare-pid",
        "--unshare-ipc",
        "--unshare-uts",
        "--unshare-net",
        "--hostname",
        "slopbox-tool",
        "--cap-drop",
        "ALL",
        "--clearenv",
        "--ro-bind",
        "/",
        "/",
        "--proc",
        "/proc",
        "--dev",
        "/dev",
        "--tmpfs",
        "/tmp",
        "--bind",
        "/run/slopbox-tool-home",
        "/home/slopbox",
        "--tmpfs",
        "/run/slopbox-pi-agent",
    ]);

    let authenticated_http_port = env::var("SLOPBOX_AUTHENTICATED_HTTP_PROXY_PORT")
        .ok()
        .map(|port| port.parse::<u16>())
        .transpose()?;
    if authenticated_http_port.is_some() {
        ensure!(
            Path::new(AUTHENTICATED_HTTP_GATEWAY_SOCKET).exists(),
            "Slopbox authenticated HTTP gateway is unavailable"
        );
    }
    match network {
        ToolNetwork::None => {
            command.args(["--tmpfs", "/run/slopbox-host/general"]);
            command.args(["--tmpfs", "/run/slopbox-host/model"]);
        }
        ToolNetwork::General => {
            ensure!(
                Path::new(GENERAL_GATEWAY_SOCKET).exists(),
                "Slopbox general gateway is unavailable"
            );
            command.args(["--tmpfs", "/run/slopbox-host/model"]);
        }
    }
    if authenticated_http_port.is_none() {
        command.args(["--tmpfs", "/run/slopbox-host/authenticated-http"]);
    }

    command
        .arg("--bind")
        .arg(&workspace)
        .arg(&workspace)
        .arg("--chdir")
        .arg(&current_dir);

    for (name, value) in tool_environment(network, authenticated_http_port)? {
        command.arg("--setenv").arg(name).arg(value);
    }

    command.arg("--");
    let general_port = if network == ToolNetwork::General {
        Some(
            env::var("SLOPBOX_PROXY_PORT")
                .context("SLOPBOX_PROXY_PORT is not set")?
                .parse::<u16>()?,
        )
    } else {
        None
    };
    if general_port.is_some() || authenticated_http_port.is_some() {
        command.arg("/run/slopbox/slopbox").arg("__tool-init");
        if let Some(port) = general_port {
            command
                .arg("--general-gateway-socket")
                .arg(GENERAL_GATEWAY_SOCKET)
                .arg("--general-proxy-port")
                .arg(port.to_string());
        }
        if let Some(port) = authenticated_http_port {
            command
                .arg("--authenticated-http-gateway-socket")
                .arg(AUTHENTICATED_HTTP_GATEWAY_SOCKET)
                .arg("--authenticated-http-proxy-port")
                .arg(port.to_string());
        }
        command.arg("--").args(child);
    } else {
        command.args(child);
    }

    super::close_inherited_descriptors(&mut command);

    command.status().context("failed to start inner bubblewrap")
}

fn tool_environment(
    network: ToolNetwork,
    authenticated_http_port: Option<u16>,
) -> Result<Vec<(OsString, OsString)>> {
    let mut values: Vec<_> = env::vars_os()
        .filter(|(name, _)| !blocked_environment_variable(name))
        .collect();
    values.extend([
        (OsString::from("HOME"), OsString::from("/home/slopbox")),
        (OsString::from("USER"), OsString::from("slopbox")),
        (OsString::from("LOGNAME"), OsString::from("slopbox")),
        (OsString::from("TMPDIR"), OsString::from("/tmp")),
        (
            OsString::from("XDG_CONFIG_HOME"),
            OsString::from("/home/slopbox/.config"),
        ),
        (
            OsString::from("XDG_CACHE_HOME"),
            OsString::from("/home/slopbox/.cache"),
        ),
        (
            OsString::from("XDG_DATA_HOME"),
            OsString::from("/home/slopbox/.local/share"),
        ),
        (
            OsString::from("XDG_STATE_HOME"),
            OsString::from("/home/slopbox/.local/state"),
        ),
    ]);

    if network == ToolNetwork::General {
        let port = env::var("SLOPBOX_PROXY_PORT")
            .context("SLOPBOX_PROXY_PORT is not set")?
            .parse::<u16>()?;
        let proxy = OsString::from(format!("http://127.0.0.1:{port}"));
        for name in ["HTTP_PROXY", "HTTPS_PROXY", "http_proxy", "https_proxy"] {
            values.push((OsString::from(name), proxy.clone()));
        }
        values.push((
            OsString::from("NO_PROXY"),
            OsString::from("127.0.0.1,localhost,::1"),
        ));
        values.push((
            OsString::from("no_proxy"),
            OsString::from("127.0.0.1,localhost,::1"),
        ));
    }

    if env::var_os("SLOPBOX_ACCOUNT_CA").is_some() {
        let ca = Path::new("/run/slopbox/account-ca.pem");
        let port = authenticated_http_port.context("account TLS broker is unavailable")?;
        ensure!(ca.is_file(), "account TLS trust is unavailable");
        values.extend([
            ("SLOPBOX_ACCOUNT_CA".into(), ca.as_os_str().to_owned()),
            (
                "SLOPBOX_ACCOUNT_PROXY".into(),
                format!("http://127.0.0.1:{port}").into(),
            ),
        ]);
    }
    if env::var_os("SLOPBOX_GITHUB_CONFIG").is_some() {
        let directory = Path::new("/run/slopbox/github");
        ensure!(
            authenticated_http_port.is_some() && directory.is_dir(),
            "GitHub account configuration is unavailable"
        );
        values.extend(crate::github::environment(directory));
    }
    if let Some(port) = authenticated_http_port {
        values.push((
            OsString::from("SLOPBOX_AUTHENTICATED_HTTP_PROXY_PORT"),
            OsString::from(port.to_string()),
        ));
        values.push((
            OsString::from("SLOPBOX_AUTHENTICATED_HTTP_BASE_URL"),
            OsString::from(format!("http://127.0.0.1:{port}")),
        ));
    }

    Ok(values)
}

fn blocked_environment_variable(name: &OsStr) -> bool {
    let name = name.as_bytes();
    name.starts_with(b"SLOPBOX_")
        || name.starts_with(b"PI_")
        || matches!(
            name,
            b"HOME"
                | b"USER"
                | b"LOGNAME"
                | b"TMPDIR"
                | b"XDG_CONFIG_HOME"
                | b"XDG_CACHE_HOME"
                | b"XDG_DATA_HOME"
                | b"XDG_STATE_HOME"
                | b"OPENROUTER_API_KEY"
                | b"HTTP_PROXY"
                | b"HTTPS_PROXY"
                | b"http_proxy"
                | b"https_proxy"
                | b"ALL_PROXY"
                | b"all_proxy"
                | b"NO_PROXY"
                | b"no_proxy"
        )
}

fn find_executable(name: &str, path: &OsStr) -> Result<PathBuf> {
    for directory in env::split_paths(path) {
        let candidate = directory.join(name);
        if candidate.is_file() {
            let canonical = fs::canonicalize(&candidate)?;
            ensure!(
                canonical.starts_with("/nix/store"),
                "required executable {name} resolves outside /nix/store"
            );
            return Ok(canonical);
        }
    }
    bail!("required executable {name} is not available in PATH")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removes_gateway_and_provider_environment() {
        for name in [
            "SLOPBOX_PROXY_PORT",
            "SLOPBOX_WORKSPACE",
            "PI_SESSION_FILE",
            "PI_CODING_AGENT_DIR",
            "OPENROUTER_API_KEY",
            "HTTP_PROXY",
            "https_proxy",
            "ALL_PROXY",
            "NO_PROXY",
        ] {
            assert!(blocked_environment_variable(OsStr::new(name)), "{name}");
        }
        assert!(!blocked_environment_variable(OsStr::new("PATH")));
        assert!(!blocked_environment_variable(OsStr::new("CARGO_HOME")));
    }
}
