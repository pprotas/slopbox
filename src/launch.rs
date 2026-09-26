use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{BufRead, IsTerminal, Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};

use crate::policy::{Policy, WorkspaceMode};

#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum Agent {
    Pi,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct LaunchConfig {
    pub workspace: PathBuf,
    pub agent: Agent,
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

pub fn save(
    config_root: &Path,
    config: &LaunchConfig,
    expected: Option<&LaunchConfig>,
) -> Result<PathBuf> {
    let path = config_path(config_root, &config.workspace);
    let parent = path.parent().expect("project configuration has a parent");
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(config_root)?;
    match DirBuilder::new().mode(0o700).create(parent) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            ensure!(
                fs::symlink_metadata(parent)?.is_dir(),
                "project configuration directory must not be a symlink: {}",
                parent.display()
            );
        }
        Err(error) => return Err(error.into()),
    }
    fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
    let lock = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path.with_extension("lock"))?;
    ensure!(
        lock.metadata()?.is_file(),
        "project configuration lock must be a regular file"
    );
    lock.lock()?;
    ensure!(
        load(config_root, &config.workspace)?.as_ref() == expected,
        "project setup changed while awaiting confirmation; run slopbox init again"
    );
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary.write_all(toml::to_string_pretty(config)?.as_bytes())?;
    temporary.as_file().sync_all()?;
    temporary
        .persist(&path)
        .with_context(|| format!("failed to save {}", path.display()))?;
    File::open(parent)?.sync_all()?;
    Ok(path)
}

pub fn terminal_text(value: &str) -> String {
    value.escape_debug().to_string()
}

pub fn require_terminal() -> Result<()> {
    ensure!(
        std::io::stdin().is_terminal() && std::io::stderr().is_terminal(),
        "project setup needs a host terminal; run slopbox init there, or use slopbox init --changes <live|staged|read-only> --yes for automation"
    );
    Ok(())
}

pub fn choose_changes(
    input: &mut impl BufRead,
    output: &mut impl Write,
    ceiling: WorkspaceMode,
    default: WorkspaceMode,
) -> Result<WorkspaceMode> {
    let choices: Vec<_> = [
        WorkspaceMode::Live,
        WorkspaceMode::Staged,
        WorkspaceMode::ReadOnly,
    ]
    .into_iter()
    .filter(|mode| *mode <= ceiling)
    .collect();
    let default = default.min(ceiling);
    writeln!(output, "How should changes work?")?;
    for (index, mode) in choices.iter().enumerate() {
        let description = match mode {
            WorkspaceMode::Live => "Work directly in this folder; host tools see edits immediately",
            WorkspaceMode::Staged => "Keep changes separate for review",
            WorkspaceMode::ReadOnly => "Read only",
        };
        writeln!(
            output,
            "  {}. {description}{}",
            index + 1,
            if *mode == default { " (default)" } else { "" }
        )?;
    }
    loop {
        write!(output, "Choice: ")?;
        output.flush()?;
        let answer = read_answer(input)?;
        if answer.is_empty() {
            return Ok(default);
        }
        if let Ok(index) = answer.parse::<usize>()
            && let Some(mode) = index.checked_sub(1).and_then(|index| choices.get(index))
        {
            return Ok(*mode);
        }
        writeln!(output, "Choose a number from 1 to {}.", choices.len())?;
    }
}

pub fn confirm(input: &mut impl BufRead, output: &mut impl Write) -> Result<bool> {
    write!(output, "Save this access policy for Pi? [y/N] ")?;
    output.flush()?;
    Ok(matches!(
        read_answer(input)?.to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

fn read_answer(input: &mut impl BufRead) -> Result<String> {
    let mut answer = String::new();
    if input.read_line(&mut answer)? == 0 {
        bail!("setup cancelled: no input received");
    }
    Ok(answer.trim().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::Profile;
    use std::io::Cursor;
    use std::os::unix::fs::symlink;

    #[test]
    fn saves_private_configuration_without_changing_global_configuration() {
        let root = tempfile::tempdir().unwrap();
        let config_root = root.path().join("config");
        let workspace = root.path().join("workspace");
        assert!(load(&config_root, &workspace).unwrap().is_none());
        assert!(!config_root.exists());
        fs::create_dir(&config_root).unwrap();
        fs::write(
            config_root.join("config.toml"),
            "# existing global policy\n",
        )
        .unwrap();
        let config = LaunchConfig {
            workspace,
            agent: Agent::Pi,
            policy: Profile::Developer.policy(),
        };
        let path = save(&config_root, &config, None).unwrap();
        assert_eq!(
            load(&config_root, &config.workspace).unwrap(),
            Some(config.clone())
        );
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(path.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            fs::read_to_string(config_root.join("config.toml")).unwrap(),
            "# existing global policy\n"
        );
        let mut replacement = config.clone();
        replacement.policy.workspace = WorkspaceMode::Staged;
        assert!(save(&config_root, &replacement, None).is_err());
        assert_eq!(
            load(&config_root, &config.workspace).unwrap(),
            Some(config.clone())
        );
        save(&config_root, &replacement, Some(&config)).unwrap();
        assert_eq!(
            load(&config_root, &config.workspace).unwrap(),
            Some(replacement)
        );
    }

    #[test]
    fn refuses_redirected_configuration_and_other_workspaces() {
        let root = tempfile::tempdir().unwrap();
        let config_root = root.path().join("config");
        let outside = root.path().join("outside");
        fs::create_dir(&config_root).unwrap();
        fs::create_dir(&outside).unwrap();
        let config = LaunchConfig {
            workspace: root.path().join("workspace"),
            agent: Agent::Pi,
            policy: Profile::Developer.policy(),
        };
        symlink(&outside, config_root.join("projects")).unwrap();
        assert!(load(&config_root, &config.workspace).is_err());
        assert!(save(&config_root, &config, None).is_err());
        assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);
        fs::remove_file(config_root.join("projects")).unwrap();
        let path = save(&config_root, &config, None).unwrap();
        let other = root.path().join("other-workspace");
        fs::copy(&path, config_path(&config_root, &other)).unwrap();
        assert!(load(&config_root, &other).is_err());
        fs::remove_file(&path).unwrap();
        symlink(outside.join("absent"), &path).unwrap();
        assert!(load(&config_root, &config.workspace).is_err());
        assert!(save(&config_root, &config, None).is_err());
    }

    #[test]
    fn rejects_unsupported_agents_and_unknown_setup_fields() {
        let root = tempfile::tempdir().unwrap();
        let config = LaunchConfig {
            workspace: root.path().join("workspace"),
            agent: Agent::Pi,
            policy: Profile::Developer.policy(),
        };
        let path = save(root.path(), &config, None).unwrap();
        let valid = fs::read_to_string(&path).unwrap();
        fs::write(
            &path,
            valid.replace("agent = \"pi\"", "agent = \"unknown\""),
        )
        .unwrap();
        assert!(load(root.path(), &config.workspace).is_err());
        fs::write(&path, format!("command = \"unapproved\"\n{valid}")).unwrap();
        assert!(load(root.path(), &config.workspace).is_err());
    }

    #[test]
    fn setup_choices_respect_the_ceiling_and_default() {
        let mut output = Vec::new();
        assert_eq!(
            choose_changes(
                &mut Cursor::new("0\n3\n2\n"),
                &mut output,
                WorkspaceMode::Staged,
                WorkspaceMode::Staged
            )
            .unwrap(),
            WorkspaceMode::ReadOnly
        );
        assert!(!String::from_utf8(output).unwrap().contains("Work directly"));
        assert_eq!(
            choose_changes(
                &mut Cursor::new("\n"),
                &mut Vec::new(),
                WorkspaceMode::ReadOnly,
                WorkspaceMode::Live
            )
            .unwrap(),
            WorkspaceMode::ReadOnly
        );
        assert!(
            choose_changes(
                &mut Cursor::new(""),
                &mut Vec::new(),
                WorkspaceMode::Live,
                WorkspaceMode::Live
            )
            .is_err()
        );
    }

    #[test]
    fn setup_text_cannot_inject_terminal_controls() {
        let escaped = terminal_text("project\n\u{1b}[2J\u{202e}");
        assert!(!escaped.contains(['\n', '\u{1b}', '\u{202e}']));
        assert!(escaped.contains("project"));
    }

    #[test]
    fn confirmation_requires_an_explicit_yes() {
        for answer in ["\n", "n\n", "anything\n"] {
            assert!(!confirm(&mut Cursor::new(answer), &mut Vec::new()).unwrap());
        }
        assert!(confirm(&mut Cursor::new(""), &mut Vec::new()).is_err());
        assert!(confirm(&mut Cursor::new("YES\n"), &mut Vec::new()).unwrap());
    }
}
