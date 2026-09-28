use std::collections::HashSet;
use std::env;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, ensure};

use super::find_executable;
use crate::DevEnvironment;
use crate::policy::RuntimeMode;

use crate::backend::nix::store_root;
use crate::backend::{PreparedDevEnvironment, RuntimePlan, nix};

pub(crate) fn host_nix() -> Result<PathBuf> {
    crate::command::trusted_executable("nix")
        .context("project flakes require a Nix-installed nix executable on host PATH")
}

fn nix_command() -> Result<Command> {
    let mut command = Command::new(host_nix()?);
    command
        .env_clear()
        .env("HOME", env::var_os("HOME").context("HOME is not set")?)
        .env("PATH", super::sandbox_path()?)
        .args(["--extra-experimental-features", "nix-command flakes"]);
    Ok(command)
}

fn system_links(path: &OsStr) -> Result<Vec<(PathBuf, PathBuf)>> {
    let mut links = vec![
        ("/bin/sh".into(), find_executable("bash", path)?),
        ("/usr/bin/env".into(), find_executable("env", path)?),
        ("/run/slopbox/bwrap".into(), find_executable("bwrap", path)?),
    ];
    if env::var_os("NIX_LD").is_some() {
        let loader = Path::new("/lib64/ld-linux-x86-64.so.2");
        if loader.exists() {
            let target = loader.canonicalize()?;
            store_root(&target).context("NIX_LD requires a Nix-store loader shim")?;
            links.push((loader.to_path_buf(), target));
        }
    }
    Ok(links)
}

pub(crate) fn prepare_dev_environment(
    session_dir: &Path,
    workspace_source: &Path,
    mode: DevEnvironment,
) -> Result<Option<PreparedDevEnvironment>> {
    if !nix::enabled(workspace_source, mode)? {
        return Ok(None);
    }

    let profile = session_dir.join("dev-profile");
    let script = session_dir.join("dev-env.sh");
    let temporary = session_dir.join(".dev-env.sh");
    let mut command = nix_command()?;
    let profile = nix::realize(&mut command, workspace_source, &profile, &temporary, false)?;

    fs::rename(&temporary, &script).with_context(|| {
        format!(
            "failed to install development environment script {}",
            script.display()
        )
    })?;
    Ok(Some(PreparedDevEnvironment { script, profile }))
}

pub(crate) fn prepare_runtime(
    mode: RuntimeMode,
    host_path: &OsStr,
    dev_environment: Option<&PreparedDevEnvironment>,
    harness_executable: Option<&Path>,
    git_signing: bool,
    dry_run: bool,
    selected: Option<RuntimePlan>,
) -> Result<RuntimePlan> {
    if let Some(selected) = selected {
        return Ok(selected);
    }
    ensure!(
        env::split_paths(host_path).all(|path| path.starts_with("/nix/store")),
        "Nix-free execution requires a host [runtime] executables selection"
    );
    let mut system_links = system_links(host_path)?;
    if mode == RuntimeMode::Host {
        let profile = Path::new("/run/current-system/sw");
        if profile.exists() {
            let target = profile.canonicalize()?;
            store_root(&target).context("host system profile resolves outside /nix/store")?;
            system_links.push((profile.to_path_buf(), target));
        }
        return Ok(RuntimePlan {
            read_only_paths: vec!["/nix/store".into()],
            system_links,
            path: host_path.to_os_string(),
        });
    }
    ensure!(
        mode == RuntimeMode::Project,
        "runtime={} is not implemented",
        mode
    );
    ensure!(
        !dry_run,
        "--dry-run cannot resolve a project runtime closure"
    );
    let dev_environment = dev_environment
        .context("runtime=project requires an activated flake development environment")?;

    let mut roots = HashSet::new();
    let mut runtime_bins = Vec::new();
    add_store_root(&mut roots, &dev_environment.profile)?;
    let mut required_executables = vec![
        find_executable("bash", host_path)?,
        find_executable("bwrap", host_path)?,
    ];
    required_executables.extend(system_links.iter().map(|(_, target)| target.clone()));
    if git_signing {
        required_executables.push(crate::command::trusted_executable("ssh-keygen")?);
    }
    for executable in required_executables {
        let executable = fs::canonicalize(executable)?;
        add_store_root(&mut roots, &executable)?;
        runtime_bins.push(
            executable
                .parent()
                .context("runtime executable has no parent directory")?
                .to_path_buf(),
        );
    }
    if let Some(executable) = harness_executable {
        let executable = fs::canonicalize(executable)?;
        add_store_root(&mut roots, &executable)?;
        runtime_bins.push(
            executable
                .parent()
                .context("harness executable has no parent directory")?
                .to_path_buf(),
        );
    }
    let current_exe = fs::canonicalize(env::current_exe()?)?;
    if current_exe.starts_with("/nix/store") {
        add_store_root(&mut roots, &current_exe)?;
    }
    if let Some(loader) = env::var_os("NIX_LD") {
        add_store_root(&mut roots, Path::new(&loader))?;
    }
    if let Some(libraries) = env::var_os("NIX_LD_LIBRARY_PATH") {
        for path in env::split_paths(&libraries) {
            add_store_root(&mut roots, &path)?;
        }
    }
    for certificate in [
        "/etc/ssl/certs/ca-bundle.crt",
        "/etc/ssl/certs/ca-certificates.crt",
        "/etc/static/ssl/certs/ca-bundle.crt",
        "/etc/static/ssl/certs/ca-certificates.crt",
    ] {
        if let Ok(certificate) = fs::canonicalize(certificate)
            && certificate.starts_with("/nix/store")
        {
            add_store_root(&mut roots, &certificate)?;
        }
    }

    let mut roots: Vec<_> = roots.into_iter().collect();
    roots.sort();
    let store_paths = query_store_closure(&roots)?;
    let allowed: HashSet<_> = store_paths.iter().cloned().collect();
    let mut path_entries = Vec::new();
    let mut seen = HashSet::new();
    for directory in runtime_bins {
        if seen.insert(directory.clone()) {
            path_entries.push(directory);
        }
    }
    for entry in env::split_paths(host_path) {
        let Ok(canonical) = fs::canonicalize(entry) else {
            continue;
        };
        let Ok(root) = store_root(&canonical) else {
            continue;
        };
        if allowed.contains(&root) && seen.insert(canonical.clone()) {
            path_entries.push(canonical);
        }
    }
    ensure!(
        !path_entries.is_empty(),
        "project runtime produced no usable PATH entries"
    );
    let path = env::join_paths(path_entries).context("failed to construct project runtime PATH")?;
    Ok(RuntimePlan {
        read_only_paths: store_paths,
        system_links,
        path,
    })
}

fn add_store_root(roots: &mut HashSet<PathBuf>, path: &Path) -> Result<()> {
    let path = fs::canonicalize(path)
        .with_context(|| format!("failed to resolve runtime path {}", path.display()))?;
    roots.insert(store_root(&path)?);
    Ok(())
}

fn query_store_closure(roots: &[PathBuf]) -> Result<Vec<PathBuf>> {
    ensure!(!roots.is_empty(), "project runtime closure has no roots");
    let output = nix_command()?
        .args(["path-info", "--recursive"])
        .args(roots)
        .output()
        .context("failed to query project runtime closure")?;
    ensure!(
        output.status.success(),
        "Nix closure query failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    nix::closure_paths(&output.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn extracts_top_level_nix_store_roots() {
        assert_eq!(
            store_root(Path::new("/nix/store/abc-package/bin/tool")).unwrap(),
            Path::new("/nix/store/abc-package")
        );
        assert!(store_root(Path::new("/usr/bin/tool")).is_err());
        assert!(store_root(Path::new("/nix/store")).is_err());
    }
}
