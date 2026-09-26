use std::fs::OpenOptions;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, ensure};

use crate::DevEnvironment;

pub(crate) fn enabled(workspace: &Path, mode: DevEnvironment) -> Result<bool> {
    let flake = workspace.join("flake.nix").is_file();
    match mode {
        DevEnvironment::Auto => Ok(flake),
        DevEnvironment::Flake => {
            ensure!(flake, "--dev-env=flake requires flake.nix");
            Ok(true)
        }
        DevEnvironment::None => Ok(false),
    }
}

#[cfg(target_os = "macos")]
pub(crate) fn command(nix: &Path) -> Result<Command> {
    use std::env;

    let mut command = Command::new(nix);
    command
        .env_clear()
        .env("HOME", env::var_os("HOME").context("HOME is not set")?)
        .env(
            "PATH",
            env::join_paths([
                nix.parent().context("Nix has no parent directory")?,
                Path::new("/usr/bin"),
                Path::new("/bin"),
            ])?,
        )
        .args(["--extra-experimental-features", "nix-command flakes"]);
    Ok(command)
}

pub(crate) fn realize(
    command: &mut Command,
    workspace: &Path,
    profile: &Path,
    output: &Path,
    json: bool,
) -> Result<PathBuf> {
    let file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(output)?;
    eprintln!(
        "slopbox: realizing trusted Nix development environment for {}",
        workspace.display()
    );
    command
        .current_dir(workspace)
        .args(["print-dev-env", "--profile"])
        .arg(profile);
    if json {
        command.args([
            "--json",
            "--no-write-lock-file",
            "--option",
            "accept-flake-config",
            "false",
        ]);
    }
    let status = command
        .arg(".#")
        .stdout(file)
        .status()
        .context("failed to run nix print-dev-env")?;
    ensure!(status.success(), "nix print-dev-env failed with {status}");
    let profile = profile
        .canonicalize()
        .context("resolve Nix development profile")?;
    store_root(&profile)?;
    Ok(profile)
}

pub(crate) fn store_root(path: &Path) -> Result<PathBuf> {
    ensure!(
        !path
            .components()
            .any(|part| matches!(part, Component::ParentDir))
            && path
                .to_str()
                .is_some_and(|text| !text.chars().any(char::is_control)),
        "Nix store paths cannot contain traversal, controls or non-UTF-8 text"
    );
    let relative = path
        .strip_prefix("/nix/store")
        .with_context(|| format!("runtime path is outside /nix/store: {}", path.display()))?;
    let Some(Component::Normal(name)) = relative.components().next() else {
        anyhow::bail!("runtime path is the Nix store root");
    };
    Ok(Path::new("/nix/store").join(name))
}

pub(crate) fn closure_paths(output: &[u8]) -> Result<Vec<PathBuf>> {
    let output = std::str::from_utf8(output).context("Nix returned non-UTF-8 paths")?;
    let mut paths = Vec::new();
    for line in output.lines() {
        let path = PathBuf::from(line);
        ensure!(
            store_root(&path)? == path && path.parent() == Some(Path::new("/nix/store")),
            "Nix returned invalid closure path {}",
            path.display()
        );
        ensure!(
            path.canonicalize()
                .with_context(|| format!("resolve closure path {}", path.display()))?
                == path,
            "Nix returned non-canonical closure path {}",
            path.display()
        );
        paths.push(path);
    }
    paths.sort();
    paths.dedup();
    ensure!(!paths.is_empty(), "Nix returned an empty closure");
    Ok(paths)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn store_paths_cannot_grant_the_store_or_escape_it() {
        assert_eq!(
            store_root(Path::new("/nix/store/abc-package/bin/tool")).unwrap(),
            Path::new("/nix/store/abc-package")
        );
        for path in [
            "/usr/bin/tool",
            "/nix/store",
            "/nix/store/../var",
            "/nix/store/package\n",
            "/nix/store-other/tool",
        ] {
            assert!(store_root(Path::new(path)).is_err(), "{path}");
        }
        for output in [
            b"".as_slice(),
            b"/nix/store\n",
            b"/nix/store/../var\n",
            b"/private\n",
            b"\xff",
        ] {
            assert!(closure_paths(output).is_err());
        }
    }

    #[test]
    fn environment_selection_does_not_evaluate_the_flake() {
        let workspace = tempfile::tempdir().unwrap();
        assert!(!enabled(workspace.path(), DevEnvironment::Auto).unwrap());
        assert!(enabled(workspace.path(), DevEnvironment::Flake).is_err());
        fs::write(
            workspace.path().join("flake.nix"),
            "throw \"must not evaluate\"",
        )
        .unwrap();
        assert!(enabled(workspace.path(), DevEnvironment::Auto).unwrap());
        assert!(enabled(workspace.path(), DevEnvironment::Flake).unwrap());
        assert!(!enabled(workspace.path(), DevEnvironment::None).unwrap());
    }
}
