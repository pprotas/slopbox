use std::fs::{self, OpenOptions};
use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use serde::Deserialize;

use crate::policy::Policy;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum LegacyAgent {
    Pi,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct LaunchConfig {
    pub workspace: PathBuf,
    // Keep reading existing records: deleting a historical launch must not discard its ceiling.
    pub agent: LegacyAgent,
    pub policy: Policy,
}

pub fn config_path(config_root: &Path, workspace: &Path) -> PathBuf {
    use std::os::unix::ffi::OsStrExt;
    let id = blake3::hash(workspace.as_os_str().as_bytes()).to_hex();
    config_root.join("projects").join(format!("{id}.toml"))
}

pub fn load(config_root: &Path, workspace: &Path) -> Result<Option<LaunchConfig>> {
    let path = config_path(config_root, workspace);
    let parent = path.parent().expect("project configuration has a parent");
    match fs::symlink_metadata(parent) {
        Ok(metadata) => ensure!(
            metadata.is_dir(),
            "project configuration directory must not be a symlink: {}",
            parent.display()
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    }
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(&path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| {
                format!("failed to read project configuration {}", path.display())
            });
        }
    };
    ensure!(
        file.metadata()?.is_file(),
        "project configuration must be a regular file"
    );
    let mut contents = String::new();
    file.take(1024 * 1024 + 1).read_to_string(&mut contents)?;
    ensure!(
        contents.len() <= 1024 * 1024,
        "project configuration is too large"
    );
    let config: LaunchConfig = toml::from_str(&contents)
        .with_context(|| format!("invalid project configuration {}", path.display()))?;
    ensure!(
        config.workspace == workspace,
        "saved project configuration belongs to another workspace"
    );
    Ok(Some(config))
}

pub fn terminal_text(value: &str) -> String {
    value.escape_debug().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::Profile;
    use std::os::unix::fs::symlink;

    #[test]
    fn reads_old_ceiling_but_never_creates_project_state() {
        let root = tempfile::tempdir().unwrap();
        let config_root = root.path().join("config");
        let workspace = root.path().join("workspace");
        assert!(load(&config_root, &workspace).unwrap().is_none());
        assert!(!config_root.exists());
        let path = config_path(&config_root, &workspace);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let policy = Profile::Developer.policy();
        fs::write(
            &path,
            format!(
                "workspace = {:?}\nagent = \"pi\"\n[policy]\n{}",
                workspace.display().to_string(),
                toml::to_string(&policy).unwrap()
            ),
        )
        .unwrap();
        assert_eq!(
            load(&config_root, &workspace).unwrap().unwrap().policy,
            policy
        );
    }

    #[test]
    fn refuses_redirected_configuration_other_workspaces_and_unknown_fields() {
        let root = tempfile::tempdir().unwrap();
        let config_root = root.path().join("config");
        let workspace = root.path().join("workspace");
        let path = config_path(&config_root, &workspace);
        fs::create_dir(&config_root).unwrap();
        symlink(root.path(), config_root.join("projects")).unwrap();
        assert!(load(&config_root, &workspace).is_err());
        fs::remove_file(config_root.join("projects")).unwrap();
        fs::create_dir(config_root.join("projects")).unwrap();
        fs::write(
            &path,
            format!(
                "workspace = {:?}\nagent = \"unknown\"\n[policy]\n{}",
                workspace.display().to_string(),
                toml::to_string(&Profile::Developer.policy()).unwrap()
            ),
        )
        .unwrap();
        assert!(load(&config_root, &workspace).is_err());
        let text = fs::read_to_string(&path).unwrap().replace("unknown", "pi");
        fs::write(&path, format!("extra = \"grant\"\n{text}")).unwrap();
        assert!(load(&config_root, &workspace).is_err());
        fs::write(&path, text).unwrap();
        assert!(
            load(&config_root, &root.path().join("other"))
                .unwrap()
                .is_none()
        );
        assert!(load(&config_root, &workspace).is_ok());
        fs::remove_file(&path).unwrap();
        symlink(root.path().join("absent"), &path).unwrap();
        assert!(load(&config_root, &workspace).is_err());
    }

    #[test]
    fn displayed_paths_escape_terminal_controls() {
        let escaped = terminal_text("project\n\u{1b}[2J\u{202e}");
        assert!(!escaped.contains(['\n', '\u{1b}', '\u{202e}']));
    }
}
