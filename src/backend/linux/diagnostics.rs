use std::ffi::OsStr;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};

use super::{find_executable, sandbox_path, validate_workspace_contents};
use crate::policy::{Backend, Policy, RuntimeMode};

pub(crate) fn check(
    workspace: &Path,
    policy: Policy,
    checks: &mut Vec<(&'static str, Result<String>)>,
    notes: &mut Vec<String>,
) {
    checks.push((
        "Workspace",
        validate_workspace_contents(workspace)
            .map(|()| "no Unix sockets or nested mounts found".into()),
    ));
    match sandbox_path() {
        Ok(host_path) => {
            checks.push((
                "Pi executable",
                executable("pi", &host_path)
                    .map(|path| format!("{} (not started)", path.display())),
            ));
            if policy.backend == Backend::Native {
                checks.push((
                    "Namespaces",
                    (|| {
                        let bwrap = executable("bwrap", &host_path)?;
                        let bash = executable("bash", &host_path)?;
                        super::probe::namespace_probe(&bwrap, &bash)
                    })(),
                ));
            } else {
                notes.push("Namespaces: native probe skipped for a non-native backend".into());
            }
        }
        Err(error) => checks.push((
            "Host PATH",
            Err(error.context(
                "install the required host Nix tools and expose their bin directories in PATH",
            )),
        )),
    }
    checks.push((
        "Development environment",
        (|| {
            ensure!(
                policy.runtime != RuntimeMode::Image,
                "runtime=image is not implemented"
            );
            let flake = workspace.join("flake.nix").is_file();
            ensure!(
                policy.runtime != RuntimeMode::Project || flake,
                "runtime=project requires flake.nix; provide a trusted project development flake"
            );
            if flake {
                let nix = super::runtime::system_nix()?;
                Ok(format!(
                    "project flake present; Nix at {} (not evaluated)",
                    nix.display()
                ))
            } else {
                Ok("no project flake; using host runtime tools".into())
            }
        })(),
    ));
}

fn executable(name: &str, path: &OsStr) -> Result<PathBuf> {
    let executable = find_executable(name, path).with_context(|| {
        format!("install {name} on the host and include its Nix bin directory in PATH")
    })?;
    let executable = fs::canonicalize(executable)?;
    ensure!(
        executable.starts_with("/nix/store"),
        "{name} resolves outside /nix/store; use a host Nix package"
    );
    ensure!(
        fs::metadata(&executable)?.permissions().mode() & 0o111 != 0,
        "{name} is not executable; repair the host package installation"
    );
    Ok(executable)
}

pub(crate) fn runtime_description(mode: RuntimeMode) -> &'static str {
    match mode {
        RuntimeMode::Host => {
            "Entire Nix store and system tools readable, including any stored source"
        }
        RuntimeMode::Project => "Selected Nix closure readable; resolved at launch",
        RuntimeMode::Image => "Isolated image requested; not implemented",
    }
}

pub(crate) fn environment_description(workspace: &Path) -> &'static str {
    if workspace.join("flake.nix").is_file() {
        "Project flake activated at launch; Nix evaluation and builds run on the host"
    } else {
        "No project flake; use the selected runtime tools"
    }
}
