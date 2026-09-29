use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::Seek;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail, ensure};

use super::{BrokerConnections, ExecutionPlan};
use crate::policy::{Policy, RuntimeMode, WorkspaceMode};

#[allow(dead_code)]
pub(crate) mod engine;
mod profile;
pub(crate) mod runtime;

pub(crate) fn unavailable<T>() -> Result<T> {
    bail!("native macOS does not use the Linux sandbox initializer (no unsandboxed fallback)")
}

pub(crate) fn ensure_supported() -> Result<()> {
    let selected = crate::session::macos_config()?;
    ensure!(
        selected.is_some(),
        "native macOS requires host-selected [runtime] resources; no unsandboxed fallback"
    );
    Ok(())
}

pub(crate) fn validate(selected: bool, policy: Policy, dry_run: bool) -> Result<()> {
    validate_policy(policy)?;
    ensure!(!dry_run, "native dry-run is not enabled");
    ensure!(
        selected,
        "native macOS requires host-selected [runtime] resources"
    );
    ensure!(
        policy.harness == crate::policy::HarnessMode::None,
        "generic native commands require harness=none"
    );
    Ok(())
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
    Ok("/usr/bin:/bin".into())
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

    pub fn command(
        &mut self,
        plan: &ExecutionPlan<'_>,
        child: &[OsString],
    ) -> Result<(engine::job::Job, Command)> {
        ensure!(
            plan.workspace.source == plan.workspace.target,
            "unsupported native execution plan"
        );
        let executable = self.executable();
        let role = engine::session::Role {
            profile: profile::render(plan, &executable)?,
            workspace: plan.workspace.source.to_owned(),
            home: plan.private_home.to_owned(),
        };
        let mut values = vec![format!(
            "PATH={}",
            plan.runtime
                .path
                .to_str()
                .context("native PATH is not UTF-8")?
        )];
        values.extend([
            "NO_PROXY=127.0.0.1,localhost,::1".into(),
            "no_proxy=127.0.0.1,localhost,::1".into(),
            "SHELL=/bin/bash".into(),
            "SLOPBOX_SANDBOX=1".into(),
            "GIT_CONFIG_NOSYSTEM=1".into(),
        ]);
        let gitconfig = plan.session_dir.join("gitconfig");
        if gitconfig.is_file() {
            values.push(format!(
                "GIT_CONFIG_GLOBAL={}",
                gitconfig.to_str().context("invalid Git config path")?
            ));
        }
        for (name, value) in plan.environment {
            let name = name.to_str().context("invalid environment name")?;
            if name == "SLOPBOX_ACCOUNT_CA" {
                let endpoint = plan
                    .brokers
                    .authenticated_http
                    .as_ref()
                    .context("account TLS broker is unavailable")?;
                values.push(format!(
                    "SLOPBOX_ACCOUNT_CA={}",
                    value.to_str().context("invalid account trust path")?
                ));
                values.push(format!(
                    "SLOPBOX_ACCOUNT_PROXY=http://127.0.0.1:{}",
                    endpoint.port
                ));
                continue;
            }
            if name == "SLOPBOX_GITHUB_CONFIG" {
                for (name, value) in crate::github::environment(Path::new(value)) {
                    values.push(format!(
                        "{}={}",
                        name.to_str().context("invalid GitHub environment name")?,
                        value.to_str().context("invalid GitHub environment value")?
                    ));
                }
                continue;
            }
            values.push(format!(
                "{name}={}",
                value.to_str().context("invalid environment value")?
            ));
        }
        let temporary = plan.session_dir.join("tmp");
        fs::DirBuilder::new().mode(0o700).create(&temporary)?;
        values.push(format!("TMPDIR={}", temporary.display()));
        values.extend(["USER=slopbox".into(), "LOGNAME=slopbox".into()]);
        values.push(format!(
            "TERM={}",
            std::env::var("TERM").unwrap_or_else(|_| "xterm-256color".into())
        ));
        let arguments = std::iter::once(OsString::from("/usr/bin/env"))
            .chain(child.iter().cloned())
            .map(|value| {
                value
                    .to_str()
                    .map(str::to_owned)
                    .context("native command arguments must be UTF-8")
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(self.engine.command(&role, &values, &arguments)?)
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
        selection: Option<&crate::backend::RuntimeSelection>,
    ) {
        if let Some(selection) = selection {
            checks.push((
                "Native executable runtime",
                runtime::selected::prepare(selection, workspace).map(|runtime| {
                    format!(
                        "{} literal runtime grants checked; application not executed",
                        runtime.native.selected_files.len()
                    )
                }),
            ));
            checks.push((
                "Native policy",
                validate_policy(policy).map(|()| "Seatbelt generic command policy".into()),
            ));
            notes.push("Generic commands share their outer role's authority; no automatic harness/tool separation. Launchd, execution and provider connectivity are not probed.".into());
            return;
        }
        checks.push((
            "Native executable runtime",
            Err(anyhow::anyhow!(
                "configure host-selected [runtime] executables"
            )),
        ));
    }
    pub(crate) fn runtime_description(_mode: RuntimeMode) -> &'static str {
        "No native executable runtime selected"
    }
    pub(crate) fn environment_description(_workspace: &Path) -> &'static str {
        "No native runtime selected"
    }
}

pub(crate) mod tool {
    use super::*;
    pub(crate) fn run(
        _network: crate::ToolNetwork,
        _command: &[OsString],
    ) -> Result<std::process::ExitStatus> {
        bail!("native macOS tool-run is not implemented; no restricted tool role is available")
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
