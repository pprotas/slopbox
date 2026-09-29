#![cfg(target_os = "macos")]

use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;
use std::process::{Command, Output};

struct Fixture {
    root: tempfile::TempDir,
    workspace: PathBuf,
    config: PathBuf,
}

impl Fixture {
    fn new(config: &str) -> Self {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        let home = root.path().join("home");
        let config_path = home.join("config/slopbox/config.toml");
        fs::create_dir(&workspace).unwrap();
        let workspace = workspace.canonicalize().unwrap();
        fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        fs::write(&config_path, config).unwrap();
        Self {
            root,
            workspace,
            config: config_path,
        }
    }

    fn run(&self, args: &[&str]) -> Output {
        let home = self.root.path().join("home");
        Command::new(env!("CARGO_BIN_EXE_slopbox"))
            .args(args)
            .env_clear()
            .env("HOME", &home)
            .env("XDG_CONFIG_HOME", home.join("config"))
            .env("XDG_DATA_HOME", home.join("data"))
            .current_dir(&self.workspace)
            .output()
            .unwrap()
    }
}

fn text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

#[test]
fn generic_status_and_doctor_do_not_initialize_application_state() {
    let fixture = Fixture::new(
        "default_command = [\"/bin/sh\"]\n[policy]\nharness = \"none\"\ncredentials = \"none\"\n[runtime]\nexecutables = [\"/bin/sh\"]\n",
    );
    for command in ["status", "doctor"] {
        let output = fixture.run(&[command]);
        assert!(output.status.success(), "{}", text(&output));
        assert!(
            text(&output).contains("Generic commands"),
            "{}",
            text(&output)
        );
        assert!(!fixture.root.path().join("home/data/slopbox/boxes").exists());
    }
    assert!(fixture.config.is_file());
}

#[test]
fn removed_pi_configuration_fails_before_application_state_changes() {
    for addition in [
        "[pi]\nread_only_mounts = []\n",
        "[policy]\nharness = \"trusted\"\n",
    ] {
        let fixture = Fixture::new(&format!(
            "default_command = [\"/bin/sh\"]\n[runtime]\nexecutables = [\"/bin/sh\"]\n{addition}"
        ));
        let output = fixture.run(&["status"]);
        assert!(!output.status.success(), "{}", text(&output));
        assert!(!fixture.root.path().join("home/data/slopbox/boxes").exists());
    }
}

#[test]
fn no_implicit_pi_launch_or_runtime_fallback() {
    let fixture = Fixture::new("");
    let output = fixture.run(&["--", "--version"]);
    assert!(!output.status.success());
    assert!(
        text(&output).contains("host-selected [runtime]"),
        "{}",
        text(&output)
    );
    let output = fixture.run(&["run", "--", "/bin/sh"]);
    assert!(!output.status.success());
    assert!(
        text(&output).contains("host-selected [runtime]"),
        "{}",
        text(&output)
    );
}

#[test]
fn saved_policy_ceiling_is_not_discarded() {
    let fixture = Fixture::new(
        "default_command = [\"/bin/sh\"]\n[policy]\nharness = \"none\"\ncredentials = \"none\"\n[runtime]\nexecutables = [\"/bin/sh\"]\n",
    );
    let id = blake3::hash(fixture.workspace.as_os_str().as_bytes()).to_hex();
    let path = fixture
        .config
        .parent()
        .unwrap()
        .join("projects")
        .join(format!("{id}.toml"));
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let workspace = fixture.workspace.display();
    fs::write(
        &path,
        format!(
            "workspace = {workspace:?}\nagent = \"pi\"\n[policy]\nworkspace = \"read-only\"\nnetwork = \"none\"\nruntime = \"host\"\nharness = \"none\"\npersistence = \"project\"\ncredentials = \"none\"\nbackend = \"native\"\n"
        ),
    )
    .unwrap();
    let output = fixture.run(&["policy"]);
    assert!(output.status.success(), "{}", text(&output));
    assert!(text(&output).contains("workspace: read-only"));
    assert!(text(&output).contains("network: none"));
}
