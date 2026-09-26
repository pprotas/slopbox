use std::collections::HashSet;
use std::env;
use std::ffi::OsStr;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, ensure};

use super::find_executable;
use crate::DevEnvironment;
use crate::policy::RuntimeMode;

use crate::backend::nix::store_root;
use crate::backend::{PreparedDevEnvironment, RuntimePlan, RuntimeStore, nix};

pub(crate) fn system_nix() -> Result<PathBuf> {
    let nix = fs::canonicalize("/run/current-system/sw/bin/nix")
        .context("project flakes require /run/current-system/sw/bin/nix on the host")?;
    ensure!(
        nix.starts_with("/nix/store"),
        "system Nix resolves outside /nix/store"
    );
    ensure!(
        nix.is_file() && fs::metadata(&nix)?.permissions().mode() & 0o111 != 0,
        "system Nix is not executable; repair the host Nix installation"
    );
    Ok(nix)
}

pub(crate) fn prepare_dev_environment(
    session_dir: &Path,
    workspace_source: &Path,
    mode: DevEnvironment,
) -> Result<Option<PreparedDevEnvironment>> {
    if !nix::enabled(workspace_source, mode)? {
        return Ok(None);
    }

    let nix = system_nix()?;

    let profile = session_dir.join("dev-profile");
    let script = session_dir.join("dev-env.sh");
    let temporary = session_dir.join(".dev-env.sh");
    let mut command = Command::new(nix);
    command
        .env_clear()
        .env("HOME", env::var_os("HOME").context("HOME is not set")?)
        .env("PATH", "/run/current-system/sw/bin");
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
) -> Result<RuntimePlan> {
    if mode == RuntimeMode::Host {
        return Ok(RuntimePlan {
            store: RuntimeStore::Host,
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
        fs::canonicalize("/bin/sh").context("failed to resolve /bin/sh")?,
        fs::canonicalize("/usr/bin/env").context("failed to resolve /usr/bin/env")?,
    ];
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
    if Path::new("/lib64/ld-linux-x86-64.so.2").exists() {
        add_store_root(
            &mut roots,
            &fs::canonicalize("/lib64/ld-linux-x86-64.so.2")?,
        )?;
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
        store: RuntimeStore::Selected(store_paths),
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
    let nix_store = Path::new("/run/current-system/sw/bin/nix-store");
    let canonical_nix_store =
        fs::canonicalize(nix_store).context("failed to resolve the system nix-store executable")?;
    ensure!(
        canonical_nix_store.starts_with("/nix/store"),
        "system nix-store resolves outside /nix/store"
    );
    // Nix uses argv[0] to select its multicall compatibility frontend.
    let output = Command::new(nix_store)
        .env_clear()
        .env("PATH", "/run/current-system/sw/bin")
        .args(["--query", "--requisites"])
        .args(roots)
        .output()
        .context("failed to query project runtime closure")?;
    ensure!(
        output.status.success(),
        "nix-store closure query failed: {}",
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
