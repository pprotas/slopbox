#![cfg(target_os = "macos")]

use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::PathBuf;
use std::process::Command;

struct Fixture {
    root: tempfile::TempDir,
    workspace: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        for path in [
            "workspace",
            "home",
            "config/slopbox",
            "data/slopbox/credentials",
        ] {
            fs::create_dir_all(root.path().join(path)).unwrap();
        }
        let workspace = workspace.canonicalize().unwrap();
        fs::write(
            workspace.join("flake.nix"),
            "throw \"PROJECT_MUST_NOT_BE_EVALUATED\"",
        )
        .unwrap();
        fs::write(
            root.path().join("config/slopbox/config.toml"),
            format!(
                r#"
[policy]
harness = "none"

[secrets.synthetic]
source = "environment"
variable = "SLOPBOX_MISSING_SECRET"

[[http_routes]]
name = "synthetic"
workspace = "{}"
upstream = "https://example.invalid/"
methods = ["GET"]
authentication = {{ type = "bearer", secret = "synthetic" }}
"#,
                workspace.display()
            ),
        )
        .unwrap();
        fs::write(
            root.path()
                .join("data/slopbox/credentials/openai-codex.json"),
            "synthetic-invalid-token-store",
        )
        .unwrap();
        Self { root, workspace }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_slopbox"));
        command
            .env_clear()
            .env("HOME", self.root.path().join("home"))
            .env("XDG_CONFIG_HOME", self.root.path().join("config"))
            .env("XDG_DATA_HOME", self.root.path().join("data"))
            .env("PATH", "/not-a-toolchain")
            .current_dir(&self.workspace);
        command
    }

    fn configure_native(&self) {
        let package = self.root.path().join("runtime/pi");
        fs::create_dir_all(package.join("dist")).unwrap();
        fs::write(
            package.join("dist/cli.js"),
            "throw new Error('must not run')",
        )
        .unwrap();
        fs::write(
            package.join("package.json"),
            r#"{"name":"@earendil-works/pi-coding-agent"}"#,
        )
        .unwrap();
        let node = self.root.path().join("runtime/node");
        fs::write(&node, "#!/bin/sh\nexit 1\n").unwrap();
        fs::set_permissions(&node, fs::Permissions::from_mode(0o755)).unwrap();
        let path = self.root.path().join("config/slopbox/config.toml");
        let original = fs::read_to_string(&path).unwrap();
        fs::write(
            path,
            format!(
                "{original}\n[macos]\nnode={:?}\npi_cli={:?}\n",
                node.canonicalize().unwrap(),
                package.join("dist/cli.js").canonicalize().unwrap()
            ),
        )
        .unwrap();
    }

    fn assert_untouched(&self) {
        assert!(!self.root.path().join("data/slopbox/boxes").exists());
        assert!(!self.root.path().join("config/slopbox/projects").exists());
        assert!(!self.workspace.join("executed").exists());
        assert_eq!(
            fs::read_to_string(
                self.root
                    .path()
                    .join("data/slopbox/credentials/openai-codex.json")
            )
            .unwrap(),
            "synthetic-invalid-token-store"
        );
    }
}

#[test]
fn inspection_does_not_execute_credential_commands() {
    let fixture = Fixture::new();
    let path = fixture.root.path().join("config/slopbox/config.toml");
    let config = fs::read_to_string(&path).unwrap().replace(
        "source = \"environment\"\nvariable = \"SLOPBOX_MISSING_SECRET\"",
        "source = \"command\"\nargv = [\"gh\", \"auth\", \"token\", \"--hostname\", \"github.com\"]",
    );
    fs::write(path, config).unwrap();
    for subcommand in ["status", "doctor", "policy"] {
        let output = fixture.command().arg(subcommand).output().unwrap();
        let error = String::from_utf8(output.stderr).unwrap();
        assert_eq!(output.status.success(), subcommand != "doctor", "{error}");
        assert!(!error.contains("secret command"), "{error}");
        assert!(!error.contains("host executable gh"), "{error}");
        fixture.assert_untouched();
    }
}

#[test]
fn status_and_doctor_are_read_only_and_do_not_resolve_accounts_or_realize_environments() {
    let fixture = Fixture::new();
    for subcommand in ["status", "doctor", "policy"] {
        let output = fixture.command().arg(subcommand).output().unwrap();
        let report = String::from_utf8(output.stdout).unwrap();
        let error = String::from_utf8(output.stderr).unwrap();
        assert_eq!(
            output.status.success(),
            subcommand != "doctor",
            "{report}\n{error}"
        );
        match subcommand {
            "status" => assert!(
                report.contains("Launch        Unsupported: native macOS launch is not enabled"),
                "{report}"
            ),
            "doctor" => assert!(
                report.contains("FAIL Native backend: native macOS launch is not enabled"),
                "{report}"
            ),
            "policy" => assert!(report.contains("status: planned"), "{report}"),
            _ => unreachable!(),
        }
        assert!(!report.contains("synthetic-invalid-token-store"));
        assert!(!error.contains("SLOPBOX_MISSING_SECRET"));
        assert!(!report.contains("install the required host Nix tools"));
        fixture.assert_untouched();
    }
}

#[test]
fn policy_and_status_preserve_native_policy_checks() {
    let fixture = Fixture::new();
    fixture.configure_native();
    let path = fixture.root.path().join("config/slopbox/config.toml");
    let config = fs::read_to_string(&path).unwrap();
    for (settings, error) in [
        ("runtime = \"host\"", None),
        ("runtime = \"project\"", None),
        (
            "workspace = \"staged\"",
            Some("native staged workspaces are not enabled"),
        ),
    ] {
        fs::write(
            &path,
            config.replace("[policy]", &format!("[policy]\n{settings}")),
        )
        .unwrap();
        for subcommand in ["policy", "status"] {
            let output = fixture.command().arg(subcommand).output().unwrap();
            let report = String::from_utf8(output.stdout).unwrap();
            let stderr = String::from_utf8(output.stderr).unwrap();
            assert!(output.status.success(), "{report}\n{stderr}");
            if subcommand == "policy" {
                let status = if error.is_some() {
                    "planned"
                } else {
                    "implemented"
                };
                assert!(report.contains(&format!("status: {status}")), "{report}");
            } else if let Some(error) = error {
                assert!(
                    report.contains(&format!("Launch        Unsupported: {error}")),
                    "{report}"
                );
            } else {
                assert!(!report.contains("Launch        Unsupported:"), "{report}");
            }
            assert!(!stderr.contains("SLOPBOX_MISSING_SECRET"), "{stderr}");
            fixture.assert_untouched();
        }
    }
}

#[test]
fn project_runtime_requires_activation_before_secrets_or_state_changes() {
    let fixture = Fixture::new();
    fixture.configure_native();
    let path = fixture.root.path().join("config/slopbox/config.toml");
    let config = fs::read_to_string(&path).unwrap();
    fs::write(
        &path,
        config.replace("[policy]", "[policy]\nruntime = \"project\""),
    )
    .unwrap();
    for mode in ["none", "auto", "flake"] {
        if mode != "none" && fixture.workspace.join("flake.nix").exists() {
            fs::remove_file(fixture.workspace.join("flake.nix")).unwrap();
        }
        let output = fixture
            .command()
            .args(["run", "--dev-env", mode, "--", "pi"])
            .output()
            .unwrap();
        let error = String::from_utf8(output.stderr).unwrap();
        assert!(!output.status.success());
        assert!(
            error.contains(if mode == "flake" {
                "--dev-env=flake requires flake.nix"
            } else {
                "runtime=project requires an activated flake"
            }),
            "{error}"
        );
        assert!(!error.contains("SLOPBOX_MISSING_SECRET"));
        fixture.assert_untouched();
    }
}

#[test]
fn approval_view_requires_a_host_terminal_before_secrets_or_state_changes() {
    let fixture = Fixture::new();
    fixture.configure_native();
    let output = fixture
        .command()
        .args(["run", "--approval-view", "--dev-env", "none", "--", "pi"])
        .output()
        .unwrap();
    let error = String::from_utf8(output.stderr).unwrap();
    assert!(!output.status.success());
    assert!(
        error.contains("approval view requires a host terminal"),
        "{error}"
    );
    assert!(!error.contains("SLOPBOX_MISSING_SECRET"), "{error}");
    assert!(output.stdout.is_empty());
    fixture.assert_untouched();
}

#[test]
fn internal_workers_reject_direct_invocation_without_reading_host_configuration() {
    let fixture = Fixture::new();
    for arguments in [
        vec![
            "__macos-exec-worker",
            "/nonexistent/control",
            "(version 1)(allow default)",
            "/",
            "/",
            "2",
        ],
        vec!["__macos-relay-worker", "/nonexistent/control", "", "", ""],
        vec![
            "__macos-harness-worker",
            "/nonexistent/control",
            "(version 1)(allow default)",
            "/",
            "/",
            "/nonexistent/environment",
        ],
    ] {
        let output = fixture.command().args(arguments).output().unwrap();
        assert!(!output.status.success());
        let error = String::from_utf8(output.stderr).unwrap();
        assert!(
            error.contains("native workers require a launchd-owned job"),
            "{error}"
        );
        assert!(output.stdout.is_empty());
        fixture.assert_untouched();
    }
}

#[test]
fn configured_launch_rejects_unsupported_capabilities_before_resolving_secrets() {
    let fixture = Fixture::new();
    fixture.configure_native();
    for (arguments, expected) in [
        (
            vec![
                "run",
                "--dev-env",
                "none",
                "--",
                "/usr/bin/touch",
                "executed",
            ],
            "supports only Pi",
        ),
        (
            vec!["run", "--dev-env", "flake", "--", "pi"],
            "required host executable nix must resolve to a Homebrew Cellar or Nix-store executable outside the workspace",
        ),
        (
            vec![
                "run",
                "--dev-env",
                "none",
                "--",
                "pi",
                "--extension",
                "evil.ts",
            ],
            "not enabled in the narrow native launcher",
        ),
        (
            vec!["run", "--dry-run", "--", "pi"],
            "dry-run is not enabled",
        ),
    ] {
        let output = fixture.command().args(&arguments).output().unwrap();
        assert!(!output.status.success(), "{arguments:?}");
        let error = String::from_utf8(output.stderr).unwrap();
        assert!(error.contains(expected), "{arguments:?}: {error}");
        assert!(!error.contains("SLOPBOX_MISSING_SECRET"), "{error}");
        fixture.assert_untouched();
    }
}

#[test]
fn unsupported_native_resources_are_reported_before_secrets_or_state_changes() {
    for (resource, expected) in [
        (
            "overlay",
            "native Pi temporary_overlay_mounts are not supported",
        ),
        (
            "credential-alias",
            "native Pi resource overlaps host credentials",
        ),
    ] {
        let fixture = Fixture::new();
        fixture.configure_native();
        let path = fixture.root.path().join("config/slopbox/config.toml");
        let mut config = fs::read_to_string(&path)
            .unwrap()
            .replace("harness = \"none\"", "harness = \"trusted\"");
        if resource == "overlay" {
            let source = fixture.root.path().join("plugin-data");
            fs::create_dir(&source).unwrap();
            config.push_str(&format!(
                "\n[[pi.temporary_overlay_mounts]]\nsource={source:?}\ntarget=\"~/.cache/plugin\"\n"
            ));
        } else {
            let home = fixture.root.path().join("home");
            fs::create_dir_all(home.join(".pi/agent")).unwrap();
            fs::create_dir(home.join(".ssh")).unwrap();
            fs::write(home.join(".ssh/key"), "credential canary").unwrap();
            symlink(home.join(".ssh"), home.join(".pi/agent/extensions")).unwrap();
        }
        fs::write(path, config).unwrap();
        for arguments in [
            vec!["run", "--dev-env", "none", "--", "pi"],
            vec!["doctor"],
            vec!["status"],
        ] {
            let output = fixture.command().args(&arguments).output().unwrap();
            let report = format!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(
                output.status.success(),
                arguments[0] == "status",
                "{report}"
            );
            assert!(report.contains(expected), "{arguments:?}: {report}");
            assert!(!report.contains("SLOPBOX_MISSING_SECRET"), "{report}");
            assert!(!report.contains("credential canary"), "{report}");
            match arguments[0] {
                "doctor" => assert!(report.contains("FAIL Pi resources:"), "{report}"),
                "status" => {
                    assert!(report.contains("Launch        Unsupported:"), "{report}");
                    assert!(!report.contains("writes discarded"), "{report}");
                }
                _ => assert!(output.stdout.is_empty()),
            }
            fixture.assert_untouched();
        }
    }
}

#[test]
fn unconfigured_launch_fails_before_secret_resolution_or_project_state_changes() {
    let fixture = Fixture::new();
    for arguments in [
        vec![],
        vec!["init", "--changes", "live", "--yes"],
        vec!["run", "--", "/usr/bin/touch", "executed"],
        vec!["run", "--dry-run", "--", "/usr/bin/touch", "executed"],
        vec![
            "run",
            "--dev-env",
            "flake",
            "--",
            "/usr/bin/touch",
            "executed",
        ],
        vec!["tool-run", "--", "/usr/bin/touch", "executed"],
        vec!["__sandbox-init", "--", "/usr/bin/touch", "executed"],
        vec!["__tool-init", "--", "/usr/bin/touch", "executed"],
    ] {
        let output = fixture.command().args(&arguments).output().unwrap();
        let error = String::from_utf8(output.stderr).unwrap();
        assert!(!output.status.success(), "{arguments:?}");
        assert!(
            error.contains("native macOS launch is not enabled"),
            "{arguments:?}: {error}"
        );
        assert!(output.stdout.is_empty());
        fixture.assert_untouched();
    }
}
