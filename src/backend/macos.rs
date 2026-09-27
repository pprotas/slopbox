use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::Seek;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;

use super::{BrokerConnections, ExecutionPlan};
use crate::policy::{Policy, RuntimeMode, WorkspaceMode};

#[allow(dead_code)]
pub(crate) mod engine;
mod profile;
pub(crate) mod runtime;

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Config {
    pub node: PathBuf,
    pub pi_cli: PathBuf,
    #[serde(default = "tool_timeout")]
    pub tool_timeout_seconds: u64,
}

fn tool_timeout() -> u64 {
    120
}

impl Config {
    pub fn resolve(&self) -> Result<Self> {
        ensure!(
            self.node.is_absolute() && self.pi_cli.is_absolute(),
            "macos.node and macos.pi_cli must be absolute host-selected paths"
        );
        let node = self.node.canonicalize().context("resolve macos.node")?;
        let pi_cli = self.pi_cli.canonicalize().context("resolve macos.pi_cli")?;
        ensure!(
            node.is_file() && node.metadata()?.mode() & 0o111 != 0,
            "macos.node must be an executable file"
        );
        ensure!(
            pi_cli.is_file() && pi_cli.ends_with("dist/cli.js"),
            "macos.pi_cli must select Pi's dist/cli.js"
        );
        let package = pi_cli.parent().unwrap().parent().unwrap();
        let manifest: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(package.join("package.json"))?)?;
        ensure!(
            manifest["name"] == "@earendil-works/pi-coding-agent",
            "macos.pi_cli is not the selected Pi package"
        );

        ensure!(
            (1..=3600).contains(&self.tool_timeout_seconds),
            "macos.tool_timeout_seconds must be 1..=3600"
        );
        Ok(Self {
            node,
            pi_cli,
            tool_timeout_seconds: self.tool_timeout_seconds,
        })
    }
}

pub(crate) fn unavailable<T>() -> Result<T> {
    bail!(
        "native macOS launch is not enabled through Linux init entry points; use the native Pi launcher described in docs/macos.md (no unsandboxed fallback)"
    )
}

pub(crate) fn ensure_supported() -> Result<()> {
    crate::session::macos_config()?.ok_or_else(|| anyhow::anyhow!("native macOS launch is not enabled: configure macos.node and macos.pi_cli in host configuration; see docs/macos.md (no unsandboxed fallback)"))?;
    Ok(())
}

pub(crate) fn validate(
    config: &Config,
    workspace: &Path,
    policy: Policy,
    command: &[OsString],
    dev_env: crate::DevEnvironment,
    dry_run: bool,
) -> Result<()> {
    let config = config.resolve()?;
    ensure!(
        !config.node.starts_with(workspace)
            && !config
                .pi_cli
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .starts_with(workspace),
        "native harness runtime must be outside the workspace"
    );
    validate_policy(policy)?;
    ensure!(!dry_run, "native dry-run is not enabled");
    let project_environment = super::nix::enabled(workspace, dev_env)?;
    ensure!(
        policy.runtime != RuntimeMode::Project || project_environment,
        "runtime=project requires an activated flake development environment"
    );
    if project_environment {
        crate::command::trusted_executable("nix", workspace)?;
    }
    ensure!(
        command.first().is_some_and(|value| value == "pi"),
        "native launch currently supports only Pi"
    );
    crate::harness::pi::validate_native_arguments(&command[1..])
}

pub(crate) fn validate_policy(policy: Policy) -> Result<()> {
    ensure!(
        matches!(policy.runtime, RuntimeMode::Host | RuntimeMode::Project),
        "native macOS requires runtime=host or runtime=project"
    );
    ensure!(
        policy.workspace != WorkspaceMode::Staged,
        "native staged workspaces are not enabled"
    );
    Ok(())
}

pub(crate) fn sandbox_path() -> Result<OsString> {
    let config = crate::session::macos_config()?.context("native runtime is not configured")?;
    let config = config.resolve()?;
    std::env::join_paths([
        config.node.parent().unwrap(),
        Path::new("/usr/bin"),
        Path::new("/bin"),
    ])
    .context("construct native harness PATH")
}

pub(crate) fn required_executor(_path: &OsStr) -> Result<PathBuf> {
    #[cfg(test)]
    if let Some(path) = std::env::var_os("SLOPBOX_TEST_SLOPBOX") {
        return Ok(PathBuf::from(path).canonicalize()?);
    }
    Ok(std::env::current_exe()?.canonicalize()?)
}

pub(crate) fn runtime_root() -> Result<PathBuf> {
    let root = PathBuf::from(format!("/private/var/tmp/slopbox-{}", unsafe {
        libc::getuid()
    }));
    match std::fs::DirBuilder::new().mode(0o700).create(&root) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    let metadata = fs::symlink_metadata(&root)?;
    ensure!(
        metadata.is_dir()
            && metadata.uid() == unsafe { libc::getuid() }
            && metadata.mode() & 0o077 == 0,
        "native runtime directory is not private"
    );
    Ok(root)
}

pub(crate) struct Session {
    pub engine: engine::session::Session,
    directory: PathBuf,
    _lock: fs::File,
}

impl Session {
    pub fn start(executable: &Path, brokers: &mut BrokerConnections) -> Result<Self> {
        let base = runtime_root()?;
        let startup = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(base.join("native.lock"))?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            match startup.try_lock() {
                Ok(()) => break,
                Err(fs::TryLockError::WouldBlock) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(std::time::Duration::from_millis(5))
                }
                Err(error) => {
                    return Err(error).context("another native session is starting or recovering");
                }
            }
        }
        let root = base.join("native");
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&root)?;
        for entry in fs::read_dir(&root)? {
            let directory = entry?.path();
            let lock = match fs::OpenOptions::new()
                .read(true)
                .write(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                .open(directory.join("lock"))
            {
                Ok(lock) => lock,
                Err(error)
                    if error.kind() == std::io::ErrorKind::NotFound
                        && !directory.join("s").exists() =>
                {
                    continue;
                }
                Err(error) => return Err(error.into()),
            };
            match lock.try_lock() {
                Ok(()) => {}
                Err(fs::TryLockError::WouldBlock) => continue,
                Err(error) => return Err(error.into()),
            }
            if !directory.join("s").exists() {
                fs::remove_dir_all(&directory)?;
                continue;
            }
            match engine::session::Session::recover(&directory.join("s")) {
                Ok(()) => fs::remove_dir_all(&directory)?,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(error) => {
                    return Err(error).context(
                        "native session recovery failed; retained state requires inspection",
                    );
                }
            }
        }
        let directory = tempfile::Builder::new()
            .prefix("run-")
            .tempdir_in(&root)?
            .keep();
        let lock = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(directory.join("lock"))?;
        lock.lock()?;
        let worker = directory.join("worker");
        // Workspace edits must not replace the trusted executable used for later workers.
        fs::copy(executable, &worker)?;
        fs::set_permissions(&worker, fs::Permissions::from_mode(0o500))?;
        let routes = engine::relay::Routes {
            general: brokers
                .general
                .as_ref()
                .map(|endpoint| endpoint.socket_dir.join("gateway.sock")),
            model: brokers
                .model
                .as_ref()
                .map(|endpoint| endpoint.socket_dir.join("gateway.sock")),
            account: brokers
                .authenticated_http
                .as_ref()
                .map(|endpoint| endpoint.socket_dir.join("gateway.sock")),
        };
        let engine = match engine::session::Session::start(worker, directory.join("s"), routes) {
            Ok(engine) => engine,
            Err(error) => {
                return Err(error).context(format!(
                    "native startup failed; state retained at {}",
                    directory.display()
                ));
            }
        };
        let ports = engine.ports()?;
        for (endpoint, port) in [
            (&mut brokers.general, ports.general),
            (&mut brokers.model, ports.model),
            (&mut brokers.authenticated_http, ports.account),
        ] {
            if let Some(endpoint) = endpoint {
                endpoint.port = port.context("native broker port is missing")?;
            }
        }
        Ok(Self {
            engine,
            directory,
            _lock: lock,
        })
    }

    pub fn executable(&self) -> PathBuf {
        self.directory.join("worker")
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }

    pub fn command(
        &mut self,
        plan: &ExecutionPlan<'_>,
        child: &[OsString],
    ) -> Result<(engine::job::Job, Command)> {
        ensure!(
            plan.workspace.source == plan.workspace.target && !plan.clipboard,
            "unsupported native execution plan"
        );
        let socket = self.engine.tool_socket();
        let executable = self.executable();
        let role = |tool| -> Result<engine::session::Role> {
            Ok(engine::session::Role {
                profile: profile::render(plan, tool, &socket, &executable)?,
                workspace: plan.workspace.source.to_owned(),
                home: if tool {
                    plan.tool_home
                } else {
                    plan.private_home
                }
                .to_owned(),
            })
        };
        let config = &plan.runtime.native.config;
        let environment = |tool: bool| -> Result<Vec<String>> {
            let mut values = vec![format!(
                "PATH={}",
                plan.runtime
                    .path
                    .to_str()
                    .context("native PATH is not UTF-8")?
            )];
            if tool {
                values = plan.runtime.native.tools.environment(plan.tool_cache)?;
            }
            values.extend([
                "NO_PROXY=127.0.0.1,localhost,::1".into(),
                "no_proxy=127.0.0.1,localhost,::1".into(),
                "SHELL=/bin/bash".into(),
                "SLOPBOX_SANDBOX=1".into(),
            ]);
            let gitconfig = plan.session_dir.join("gitconfig");
            if gitconfig.is_file() {
                values.retain(|value| {
                    !value.starts_with("GIT_CONFIG_GLOBAL=")
                        && !value.starts_with("GIT_CONFIG_NOSYSTEM=")
                });
                values.extend([
                    "GIT_CONFIG_NOSYSTEM=1".into(),
                    format!(
                        "GIT_CONFIG_GLOBAL={}",
                        gitconfig.to_str().context("invalid Git config path")?
                    ),
                ]);
            }
            for (name, value) in plan.environment.iter().chain(if tool {
                &[][..]
            } else {
                &plan.harness.environment
            }) {
                let name = name.to_str().context("invalid environment name")?;
                if name == "SLOPBOX_ACCOUNT_CA" {
                    if tool {
                        let endpoint = plan
                            .brokers
                            .authenticated_http
                            .as_ref()
                            .context("account TLS broker is unavailable")?;
                        values.extend([
                            format!(
                                "SLOPBOX_ACCOUNT_CA={}",
                                value.to_str().context("invalid account trust path")?
                            ),
                            format!("SLOPBOX_ACCOUNT_PROXY=http://127.0.0.1:{}", endpoint.port),
                        ]);
                    }
                    continue;
                }
                if name == "SLOPBOX_GITHUB_CONFIG" {
                    if tool {
                        for (name, value) in crate::github::environment(Path::new(value)) {
                            values.push(format!(
                                "{}={}",
                                name.to_str().context("invalid GitHub environment name")?,
                                value.to_str().context("invalid GitHub environment value")?
                            ));
                        }
                    }
                    continue;
                }
                if tool
                    && (name.starts_with("SLOPBOX_MODEL")
                        || (matches!(
                            name,
                            "HTTP_PROXY"
                                | "HTTPS_PROXY"
                                | "http_proxy"
                                | "https_proxy"
                                | "SLOPBOX_PROXY_PORT"
                        ) && !plan.harness.environment.iter().any(|(key, value)| {
                            key == "SLOPBOX_PI_TOOL_NETWORK" && value == "general"
                        })))
                {
                    continue;
                }
                values.push(format!(
                    "{name}={}",
                    value.to_str().context("invalid environment value")?
                ));
            }
            Ok(values)
        };
        let activation = match plan.dev_environment {
            Some(environment) => vec![
                environment
                    .bash
                    .to_str()
                    .context("invalid Nix Bash path")?
                    .to_owned(),
                "--noprofile".into(),
                "--norc".into(),
                environment
                    .script
                    .to_str()
                    .context("invalid Nix activation path")?
                    .to_owned(),
            ],
            None => Vec::new(),
        };
        self.engine.start_tools_configured(
            role(true)?,
            &environment(true)?,
            std::time::Duration::from_secs(config.tool_timeout_seconds),
            activation,
        )?;
        let mut harness_environment = environment(false)?;
        harness_environment.extend([
            format!(
                "SLOPBOX_NATIVE_SOCKET={}",
                self.engine.tool_socket().display()
            ),
            format!(
                "SLOPBOX_NATIVE_WORKSPACE={}",
                plan.workspace.source.display()
            ),
            format!("SLOPBOX_NATIVE_TIMEOUT={}", config.tool_timeout_seconds + 5),
            format!(
                "TERM={}",
                std::env::var("TERM").unwrap_or_else(|_| "xterm-256color".into())
            ),
        ]);
        if let Ok(value) = std::env::var("COLORTERM") {
            harness_environment.push(format!("COLORTERM={value}"));
        }
        let mut arguments = vec![
            "/bin/bash".into(),
            plan.session_dir
                .join("pi-wrapper")
                .to_str()
                .context("invalid wrapper path")?
                .into(),
        ];
        arguments.extend(
            child[1..]
                .iter()
                .map(|value| {
                    value
                        .to_str()
                        .map(str::to_owned)
                        .context("invalid Pi argument")
                })
                .collect::<Result<Vec<_>>>()?,
        );
        Ok(self
            .engine
            .command(&role(false)?, &harness_environment, &arguments)?)
    }

    pub fn finish(&mut self) -> Result<()> {
        self.engine.finish()?;
        if self.directory.exists() {
            fs::remove_dir_all(&self.directory)?;
        }
        Ok(())
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if let Err(error) = self.finish() {
            eprintln!("slopbox: native cleanup incomplete: {error:#}");
        }
    }
}

pub(crate) fn validate_workspace_contents(_workspace: &Path) -> Result<()> {
    Ok(())
}
pub(crate) fn validate_workspace_target(workspace: &Path) -> Result<()> {
    ensure!(
        workspace.is_absolute(),
        "native workspace must be an absolute host path"
    );
    let control = PathBuf::from(format!("/private/var/tmp/slopbox-{}", unsafe {
        libc::getuid()
    }));
    ensure!(
        !control.starts_with(workspace),
        "workspace contains native control state"
    );
    Ok(())
}
pub(crate) fn clone_or_copy_file(
    input: &mut fs::File,
    output: &mut fs::File,
) -> std::io::Result<()> {
    input.rewind()?;
    output.set_len(0)?;
    output.rewind()?;
    std::io::copy(input, output)?;
    Ok(())
}

pub(crate) mod diagnostics {
    use super::*;
    pub(crate) fn check(
        workspace: &Path,
        policy: Policy,
        checks: &mut Vec<(&'static str, Result<String>)>,
        notes: &mut Vec<String>,
    ) {
        checks.push((
            "Native backend",
            (|| {
                ensure_supported()?;
                let config = crate::session::macos_config()?
                    .context("missing native runtime")?
                    .resolve()?;
                ensure!(
                    !config.node.starts_with(workspace)
                        && !config
                            .pi_cli
                            .parent()
                            .unwrap()
                            .parent()
                            .unwrap()
                            .starts_with(workspace),
                    "native harness runtime must be outside the workspace"
                );
                validate_policy(policy)?;
                Ok("experimental Seatbelt / launchd; explicit Node and Pi paths checked; toolchain execution not tested".into())
            })(),
        ));
        checks.push((
            "Project environment",
            (|| {
                let flake = super::super::nix::enabled(workspace, crate::DevEnvironment::Auto)?;
                ensure!(
                    policy.runtime != RuntimeMode::Project || flake,
                    "runtime=project requires flake.nix"
                );
                if flake {
                    let nix = crate::command::trusted_executable("nix", workspace)?;
                    Ok(format!(
                        "Nix at {}; project not evaluated, closure not checked",
                        nix.display()
                    ))
                } else {
                    Ok("no project flake; using host runtime tools".into())
                }
            })(),
        ));
        notes.push("Native launch is experimental; selected project closures or recognized developer installations are read-only and tool-only; arbitrary PATH roots and host tool configuration are not imported; configured Git identity and account routes are brokered; --approval-view enables host network approvals in a foreground terminal; clipboard is not enabled.".into());
    }
    pub(crate) fn runtime_description(mode: RuntimeMode) -> &'static str {
        match mode {
            RuntimeMode::Project => "Host-selected Node/Pi; selected Nix closure for tools only",
            _ => {
                "Host-selected Node/Pi; recognized host tools, with project Nix tools first when activated"
            }
        }
    }
    pub(crate) fn environment_description(workspace: &Path) -> &'static str {
        if workspace.join("flake.nix").is_file() {
            "Project flake realized on the host; activation/hooks run only in sandboxed tools"
        } else {
            "No project flake; use the selected runtime tools"
        }
    }
}

pub(crate) mod tool {
    use super::*;
    pub(crate) fn run(
        _network: crate::ToolNetwork,
        _command: &[OsString],
    ) -> Result<std::process::ExitStatus> {
        bail!(
            "native macOS launch is not enabled through tool-run; Pi uses the session-owned supervisor"
        )
    }
}
pub(crate) mod init {
    use super::*;
    pub(crate) fn run(
        _a: Option<&Path>,
        _b: Option<u16>,
        _c: Option<&Path>,
        _d: Option<u16>,
        _e: Option<&Path>,
        _f: Option<u16>,
        _command: &[OsString],
    ) -> Result<std::process::ExitStatus> {
        unavailable()
    }
    pub(crate) fn run_tool(
        _a: Option<&Path>,
        _b: Option<u16>,
        _c: Option<&Path>,
        _d: Option<u16>,
        _command: &[OsString],
    ) -> Result<std::process::ExitStatus> {
        unavailable()
    }
    pub(crate) fn print_denials(_port: u16) -> Result<()> {
        bail!("use slopbox network events from the host on macOS")
    }
}
