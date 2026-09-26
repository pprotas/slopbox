use super::*;
use serde_json::json;
use std::process::Command;

#[test]
fn activation_filters_control_state_and_only_runs_hooks_when_executed() {
    let root = tempfile::tempdir().unwrap();
    let environment: Environment = serde_json::from_value(json!({
        "variables": {
            "HOME": {"type": "exported", "value": "/host-home"},
            "TMPDIR": {"type": "exported", "value": "/host-tmp"},
            "CARGO_HOME": {"type": "exported", "value": "/host-cargo"},
            "HTTP_PROXY": {"type": "exported", "value": "host-proxy"},
            "SLOPBOX_MODEL_PROXY_PORT": {"type": "exported", "value": "12345"},
            "SSH_AUTH_SOCK": {"type": "exported", "value": "/host-agent"},
            "GIT_CONFIG_GLOBAL": {"type": "exported", "value": "/host-git"},
            "GH_CONFIG_DIR": {"type": "exported", "value": "/host-gh"},
            "GH_TOKEN": {"type": "exported", "value": "/host-token"},
            "GITHUB_TOKEN": {"type": "exported", "value": "/host-other-token"},
            "BASH_ENV": {"type": "exported", "value": "/host-script"},
            "DYLD_INSERT_LIBRARIES": {"type": "exported", "value": "/host-library"},
            "PATH": {"type": "exported", "value": "/host-profile"},
            "CC": {"type": "exported", "value": "/nix/store/compiler/bin/clang"},
            "SDKROOT": {"type": "exported", "value": "/nix/store/sdk"},
            "data": {"type": "array", "value": ["$(touch injected)", "apostrophe'", "two words"]},
            "lookup": {"type": "associative", "value": {"key": "value with spaces"}},
            "shellHook": {"type": "var", "value": "fixture_hook; export HOOK_RESULT=activated"}
        },
        "bashFunctions": {"fixture_hook": "printf '%s\\n' \"${data[@]}\" \"${lookup[key]}\" > hook"}
    }))
    .unwrap();
    let bash = crate::command::find_optional_executable("bash", &std::env::var_os("PATH").unwrap())
        .unwrap();
    let script = root.path().join("activate.sh");
    let text = environment.render("/usr/bin:/bin", &bash).unwrap();
    assert!(!text.contains("/host-"));
    fs::write(&script, text).unwrap();
    assert!(!root.path().join("hook").exists());
    let output = Command::new(&bash)
        .args(["--noprofile", "--norc"])
        .arg(script)
        .args(["/bin/bash", "--noprofile", "--norc", "-c", "printf '%s\\n' \"$HOME\" \"$CARGO_HOME\" \"$HTTP_PROXY\" \"$GIT_CONFIG_GLOBAL\" \"$HOOK_RESULT\" \"$CC\" \"$SDKROOT\" \"$NIX_BUILD_TOP\" \"${SLOPBOX_MODEL_PROXY_PORT-unset}\" \"$GH_CONFIG_DIR\" \"$GH_TOKEN\" \"$GITHUB_TOKEN\""])
        .env_clear()
        .env("HOME", "/private-home")
        .env("TMPDIR", "/private-tmp")
        .env("CARGO_HOME", "/private-cargo")
        .env("HTTP_PROXY", "broker")
        .env("GIT_CONFIG_GLOBAL", "/private-git")
        .env("GH_CONFIG_DIR", "/private-gh")
        .env("GH_TOKEN", "synthetic")
        .env("GITHUB_TOKEN", "alternate-synthetic")
        .current_dir(root.path())
        .output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "/private-home\n/private-cargo\nbroker\n/private-git\nactivated\n/nix/store/compiler/bin/clang\n/nix/store/sdk\n/private-tmp\nunset\n/private-gh\nsynthetic\nalternate-synthetic\n"
    );
    assert_eq!(
        fs::read_to_string(root.path().join("hook")).unwrap(),
        "$(touch injected)\napostrophe'\ntwo words\nvalue with spaces\n"
    );
    assert!(!root.path().join("injected").exists());
}

#[test]
fn hook_failure_does_not_run_the_requested_command() {
    let root = tempfile::tempdir().unwrap();
    for hook in ["false", "set -e; false; echo ran > hook-after-failure"] {
        let environment: Environment = serde_json::from_value(json!({
            "variables": {"shellHook": {"type": "var", "value": hook}},
            "bashFunctions": {}
        }))
        .unwrap();
        let script = root.path().join("activate.sh");
        fs::write(
            &script,
            environment
                .render("/usr/bin:/bin", Path::new("/bin/bash"))
                .unwrap(),
        )
        .unwrap();
        let output = Command::new("/bin/bash")
            .arg(script)
            .args(["/bin/bash", "-c", "echo ran > command"])
            .env_clear()
            .env("TMPDIR", root.path())
            .current_dir(root.path())
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert!(!root.path().join("command").exists());
        assert!(!root.path().join("hook-after-failure").exists());
    }
}

#[test]
fn malformed_environment_and_ambient_paths_are_rejected() {
    for name in ["", "1bad", "a; touch bad", "a\n", "$(bad)"] {
        let environment = Environment {
            variables: BTreeMap::from([(name.into(), Variable::Local("value".into()))]),
            functions: BTreeMap::new(),
            structured: None,
        };
        assert!(environment.render("/bin", Path::new("/bin/bash")).is_err());
        let environment = Environment {
            variables: BTreeMap::new(),
            functions: BTreeMap::from([(name.into(), "true".into())]),
            structured: None,
        };
        assert!(environment.render("/bin", Path::new("/bin/bash")).is_err());
    }
    let environment: Environment = serde_json::from_value(json!({
        "variables": {"variable": {"type": "exported", "value": "nul\u{0000}"}}, "bashFunctions": {}
    }))
    .unwrap();
    assert!(environment.render("/bin", Path::new("/bin/bash")).is_err());
    let structured: Environment = serde_json::from_value(json!({
        "variables": {}, "bashFunctions": {}, "structuredAttrs": {".attrs.sh": "echo unsupported"}
    }))
    .unwrap();
    assert!(structured.render("/bin", Path::new("/bin/bash")).is_err());
    assert!(tool_path(".:/bin:/usr/bin:/nix/var/nix/profiles/default/bin", &[]).is_err());
    assert!(selected_path(Path::new("/bin/bash"), &[]).is_err());
    assert!(
        serde_json::from_value::<Environment>(
            json!({"variables": {"x": {"type": "unknown"}}, "bashFunctions": {}})
        )
        .is_err()
    );
}
