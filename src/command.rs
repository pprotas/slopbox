use std::env;
use std::ffi::OsStr;
use std::fs;
#[cfg(target_os = "macos")]
use std::path::Path;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};

pub(crate) fn system_diff() -> Result<PathBuf> {
    #[cfg(target_os = "linux")]
    {
        trusted_executable("diff")
    }
    #[cfg(target_os = "macos")]
    {
        let path = "/usr/bin/diff";
        let diff = fs::canonicalize(path).context("required executable diff is not available")?;
        anyhow::ensure!(
            diff == Path::new(path),
            "system diff must be the native /usr/bin/diff"
        );
        Ok(diff)
    }
}

pub(crate) fn trusted_executable(
    name: &str,
    #[cfg(target_os = "macos")] workspace: &Path,
) -> Result<PathBuf> {
    let path = env::var_os("PATH").context("PATH is not set")?;
    #[cfg(target_os = "linux")]
    {
        nix_executable(name, &path)
    }
    #[cfg(target_os = "macos")]
    native_executable(
        name,
        &path,
        workspace,
        &[
            Path::new("/opt/homebrew/Cellar"),
            Path::new("/usr/local/Cellar"),
        ],
    )
}

#[cfg(target_os = "linux")]
pub(crate) fn nix_executable(name: &str, path: &OsStr) -> Result<PathBuf> {
    use std::os::unix::fs::PermissionsExt;

    for directory in env::split_paths(path).filter(|path| path.is_absolute()) {
        let Ok(directory) = directory.canonicalize() else {
            continue;
        };
        if !directory.starts_with("/nix/store") {
            continue;
        }
        let Ok(canonical) = directory.join(name).canonicalize() else {
            continue;
        };
        let metadata = fs::metadata(&canonical)?;
        if canonical.starts_with("/nix/store")
            && metadata.is_file()
            && metadata.permissions().mode() & 0o111 != 0
        {
            return Ok(canonical);
        }
    }
    bail!("required host executable {name} is not available in a trusted Nix PATH entry")
}

#[cfg(target_os = "macos")]
fn native_executable(
    name: &str,
    path: &OsStr,
    workspace: &Path,
    cellars: &[&Path],
) -> Result<PathBuf> {
    use std::os::unix::fs::PermissionsExt;

    let workspace = workspace
        .canonicalize()
        .context("resolve helper workspace")?;
    for directory in env::split_paths(path) {
        let candidate = directory.join(name);
        let Ok(canonical) = candidate.canonicalize() else {
            continue;
        };
        let metadata = fs::metadata(&canonical)?;
        if !metadata.is_file() || metadata.permissions().mode() & 0o111 == 0 {
            continue;
        }
        let store = Path::new("/nix/store");
        if canonical.starts_with(store)
            && !workspace.starts_with(store)
            && !store.starts_with(&workspace)
        {
            return Ok(canonical);
        }
        for cellar in cellars {
            let Ok(relative) = canonical.strip_prefix(cellar) else {
                continue;
            };
            // A package/version below Cellar, never an unpackaged prefix/bin helper.
            if relative.components().count() < 3 {
                continue;
            }
            let prefix = cellar.parent().context("invalid Homebrew Cellar")?;
            if workspace.starts_with(prefix) || prefix.starts_with(&workspace) {
                continue;
            }
            return Ok(canonical);
        }
    }
    bail!(
        "required host executable {name} must resolve to a Homebrew Cellar or Nix-store executable outside the workspace"
    )
}

pub(crate) fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

pub(crate) fn find_optional_executable(name: &str, path: &OsStr) -> Option<PathBuf> {
    env::split_paths(path)
        .map(|directory| directory.join(name))
        .find(|candidate| candidate.is_file())
}

#[cfg(all(test, target_os = "linux"))]
mod linux_tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};

    #[test]
    fn host_helpers_reject_executables_and_symlinks_outside_the_store() {
        let root = tempfile::tempdir().unwrap();
        let helper = root.path().join("nix");
        fs::write(&helper, "#!/bin/sh\nexit 99\n").unwrap();
        fs::set_permissions(&helper, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(nix_executable("nix", root.path().as_os_str()).is_err());
        symlink(&helper, root.path().join("diff")).unwrap();
        assert!(nix_executable("diff", root.path().as_os_str()).is_err());
        assert!(nix_executable("nix", OsStr::new("")).is_err());
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};

    #[test]
    fn native_credential_helpers_require_packaged_executables() {
        let root = tempfile::tempdir().unwrap();
        let base = root.path().canonicalize().unwrap();
        let workspace = base.join("workspace");
        let prefix = base.join("brew");
        let cellar = prefix.join("Cellar");
        let package = cellar.join("sops/1/bin");
        for path in [&workspace, &package, &prefix.join("bin")] {
            fs::create_dir_all(path).unwrap();
        }
        let packaged = package.join("sops");
        fs::write(&packaged, "not executed").unwrap();
        fs::set_permissions(&packaged, fs::Permissions::from_mode(0o755)).unwrap();
        let local = workspace.join("sops");
        fs::write(&local, "not executed").unwrap();
        fs::set_permissions(&local, fs::Permissions::from_mode(0o755)).unwrap();
        symlink(&packaged, prefix.join("bin/sops")).unwrap();
        let path = env::join_paths([&workspace, &prefix.join("bin")]).unwrap();
        assert_eq!(
            native_executable("sops", &path, &workspace, &[&cellar]).unwrap(),
            packaged
        );
        assert!(native_executable("sops", workspace.as_os_str(), &workspace, &[&cellar]).is_err());
        fs::set_permissions(&packaged, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(native_executable("sops", &path, &workspace, &[&cellar]).is_err());
        fs::remove_file(prefix.join("bin/sops")).unwrap();
        fs::copy(&local, prefix.join("bin/sops")).unwrap();
        assert!(native_executable("sops", &path, &workspace, &[&cellar]).is_err());
        let unversioned = cellar.join("unversioned");
        fs::create_dir(&unversioned).unwrap();
        fs::copy(&local, unversioned.join("sops")).unwrap();
        assert!(
            native_executable("sops", unversioned.as_os_str(), &workspace, &[&cellar]).is_err()
        );
    }

    #[test]
    fn native_credential_helpers_reject_package_escapes_and_writable_installations() {
        let root = tempfile::tempdir().unwrap();
        let base = root.path().canonicalize().unwrap();
        let workspace = base.join("workspace");
        let prefix = base.join("brew");
        let cellar = prefix.join("Cellar");
        let package = cellar.join("sops/1/bin");
        fs::create_dir_all(&workspace).unwrap();
        fs::create_dir_all(&package).unwrap();
        let local = workspace.join("sops");
        fs::write(&local, "not executed").unwrap();
        fs::set_permissions(&local, fs::Permissions::from_mode(0o755)).unwrap();
        symlink(&local, package.join("sops")).unwrap();
        assert!(native_executable("sops", package.as_os_str(), &workspace, &[&cellar]).is_err());
        fs::remove_file(package.join("sops")).unwrap();
        fs::copy(&local, package.join("sops")).unwrap();
        for writable in [&base, &prefix, &cellar, &package, &prefix.join("opt")] {
            fs::create_dir_all(writable).unwrap();
            assert!(native_executable("sops", package.as_os_str(), writable, &[&cellar]).is_err());
        }
        let alias = base.join("workspace-alias");
        symlink(&prefix, &alias).unwrap();
        assert!(native_executable("sops", package.as_os_str(), &alias, &[&cellar]).is_err());
        let alias = base.join("cellar-alias");
        symlink(&cellar, &alias).unwrap();
        assert!(native_executable("sops", package.as_os_str(), &workspace, &[&alias]).is_err());
        assert_eq!(
            native_executable("sops", package.as_os_str(), &workspace, &[&cellar]).unwrap(),
            package.join("sops")
        );
    }

    #[test]
    fn native_diff_accepts_stage_labels_without_using_path() {
        let root = tempfile::tempdir().unwrap();
        let before = root.path().join("before");
        let after = root.path().join("after");
        fs::write(&before, "before\n").unwrap();
        fs::write(&after, "after\n").unwrap();
        let output = std::process::Command::new(system_diff().unwrap())
            .env_clear()
            .env("PATH", "/not-a-toolchain")
            .args(["-u", "-L", "a/file", "-L", "b/file", "--"])
            .arg(before)
            .arg(after)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1));
        let diff = String::from_utf8(output.stdout).unwrap();
        assert!(diff.starts_with("--- a/file\n+++ b/file\n"), "{diff}");
        assert!(diff.contains("-before\n+after\n"), "{diff}");
    }
}
