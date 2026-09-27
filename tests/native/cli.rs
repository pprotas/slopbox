use super::*;

#[test]
#[ignore = "subprocess fixture for the native CLI integration test"]
fn native_cli_fixture() {
    assert!(
        std::env::var_os("SLOPBOX_TEST_SLOPBOX").is_some(),
        "SLOPBOX_TEST_SLOPBOX must select the production worker binary"
    );
    assert!(
        std::env::var_os("SLOPBOX_TEST_OPENROUTER").is_some()
            || std::env::var_os("SLOPBOX_TEST_ACCOUNT").is_some()
    );
    let cli = Cli::try_parse_from([
        "slopbox",
        "run",
        "--dev-env",
        "none",
        "--",
        "pi",
        "--mode",
        "rpc",
        "--provider",
        "openrouter",
        "--model",
        "openai/gpt-4o",
        "--thinking",
        "off",
    ])
    .unwrap();
    run_cli(cli).unwrap();
}

#[test]
#[ignore = "host integration: set SLOPBOX_TEST_NODE and SLOPBOX_TEST_SLOPBOX"]
fn native_cli_generic_commands() {
    let output = std::process::Command::new("/usr/bin/python3")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/native/generic.py"
        ))
        .arg(std::env::var_os("SLOPBOX_TEST_SLOPBOX").expect("built Slopbox path required"))
        .arg(std::env::var_os("SLOPBOX_TEST_NODE").expect("reviewed Node path required"))
        .env_clear()
        .current_dir("/")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("command exit-status propagation"));
}

#[test]
#[ignore = "host integration: set SLOPBOX_TEST_CLAUDE, SLOPBOX_TEST_NODE and SLOPBOX_TEST_SLOPBOX"]
fn native_cli_claude_harness() {
    let output = std::process::Command::new("/usr/bin/python3")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/claude-harness.py"
        ))
        .arg(std::env::var_os("SLOPBOX_TEST_SLOPBOX").expect("built Slopbox path required"))
        .arg(std::env::var_os("SLOPBOX_TEST_CLAUDE").expect("reviewed Claude path required"))
        .arg(std::env::var_os("SLOPBOX_TEST_NODE").expect("reviewed Node path required"))
        .arg(std::env::current_exe().unwrap())
        .env_clear()
        .current_dir("/")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("two workspaces"));
}

#[test]
#[ignore = "subprocess fixture for native generic commands with a disposable TLS CA"]
fn native_generic_fixture() {
    assert!(
        std::env::var_os("SLOPBOX_TEST_SLOPBOX").is_some(),
        "production worker required"
    );
    assert!(
        std::env::var_os("SLOPBOX_TEST_TLS_CA").is_some(),
        "disposable fixture CA required"
    );
    let arguments: Vec<String> =
        serde_json::from_str(&std::env::var("SLOPBOX_TEST_NATIVE_ARGS").unwrap()).unwrap();
    let cli = Cli::try_parse_from(std::iter::once("slopbox".to_owned()).chain(arguments)).unwrap();
    run_cli(cli).unwrap();
}

#[test]
fn native_cli_fixture_requires_a_production_worker() {
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "native_cli_tests::native_cli_fixture",
            "--exact",
            "--ignored",
            "--nocapture",
        ])
        .env_clear()
        .env("SLOPBOX_TEST_ACCOUNT", "127.0.0.1:1")
        .current_dir("/")
        .output()
        .unwrap();
    assert!(!output.status.success());
    let error = String::from_utf8(output.stderr).unwrap();
    assert!(
        error.contains("SLOPBOX_TEST_SLOPBOX must select the production worker binary"),
        "{error}"
    );
}

#[test]
#[ignore = "integration: set SLOPBOX_TEST_NODE, SLOPBOX_TEST_PI_CLI and SLOPBOX_TEST_SLOPBOX"]
fn native_terminal_round_trip() {
    let output = std::process::Command::new("/usr/bin/python3")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/native/terminal.py"
        ))
        .arg(std::env::var_os("SLOPBOX_TEST_SLOPBOX").expect("built Slopbox path required"))
        .arg(std::env::var_os("SLOPBOX_TEST_NODE").expect("reviewed Node path required"))
        .arg(std::env::var_os("SLOPBOX_TEST_PI_CLI").expect("reviewed Pi path required"))
        .env_clear()
        .current_dir("/")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("native terminal !/!!"));
}

#[test]
#[ignore = "host integration: native approval PTY; set SLOPBOX_TEST_NODE and SLOPBOX_TEST_SLOPBOX"]
fn native_approval_view_round_trip() {
    let output = std::process::Command::new("/usr/bin/python3")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/native/approvals.py"
        ))
        .arg(std::env::var_os("SLOPBOX_TEST_SLOPBOX").expect("built Slopbox path required"))
        .arg(std::env::var_os("SLOPBOX_TEST_NODE").expect("reviewed Node path required"))
        .env_clear()
        .current_dir("/")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("native host approval view passed"));
}

#[test]
fn native_git_signing_helper_accepts_explicit_socket() {
    let cli = Cli::try_parse_from([
        "slopbox",
        "__git-sign",
        "--socket",
        "/private/socket",
        "--",
        "-Y",
        "sign",
        "-n",
        "git",
        "-f",
        "public",
        "-U",
        "payload",
    ])
    .unwrap();
    let Some(Command::GitSign { socket, arguments }) = cli.command else {
        panic!("wrong command")
    };
    assert_eq!(socket, PathBuf::from("/private/socket"));
    assert_eq!(
        arguments,
        ["-Y", "sign", "-n", "git", "-f", "public", "-U", "payload"].map(OsString::from)
    );
}

#[test]
#[ignore = "host integration: disposable SSH agent, Homebrew Git and reviewed SLOPBOX_TEST_* paths"]
fn native_cli_git_signing_and_routes() {
    let output = std::process::Command::new(
        std::env::var_os("SLOPBOX_TEST_NODE").expect("reviewed Node path required"),
    )
    .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/native/git.mjs"))
    .arg(std::env::current_exe().unwrap())
    .arg(std::env::var_os("SLOPBOX_TEST_SLOPBOX").expect("built Slopbox path required"))
    .arg(std::env::var_os("SLOPBOX_TEST_PI_CLI").expect("reviewed Pi path required"))
    .env_clear()
    .current_dir("/")
    .output()
    .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout)
            .contains("native Git signing and account routes passed")
    );
}

#[test]
#[ignore = "host integration: account TLS; set SLOPBOX_TEST_NODE and SLOPBOX_TEST_SLOPBOX"]
fn native_cli_shared_accounts() {
    let output = std::process::Command::new(
        std::env::var_os("SLOPBOX_TEST_NODE").expect("reviewed Node path required"),
    )
    .arg(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/native/accounts.mjs"
    ))
    .arg(std::env::current_exe().unwrap())
    .arg(std::env::var_os("SLOPBOX_TEST_SLOPBOX").expect("built Slopbox path required"))
    .env_clear()
    .current_dir("/")
    .output()
    .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout)
            .contains("native shared accounts, curl/Node TLS mediation and enforcement passed")
    );
}

#[test]
#[ignore = "host integration: native gh broker; set SLOPBOX_TEST_NODE, SLOPBOX_TEST_GH and SLOPBOX_TEST_SLOPBOX"]
fn native_cli_github_account() {
    let output = std::process::Command::new(
        std::env::var_os("SLOPBOX_TEST_NODE").expect("reviewed Node path required"),
    )
    .arg(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/native/github.mjs"
    ))
    .arg(std::env::current_exe().unwrap())
    .arg(std::env::var_os("SLOPBOX_TEST_SLOPBOX").expect("built Slopbox path required"))
    .arg(std::env::var_os("SLOPBOX_TEST_GH").expect("reviewed gh path required"))
    .env_clear()
    .current_dir("/")
    .output()
    .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout)
            .contains("native gh REST/GraphQL, host credential command and account denials passed")
    );
}

#[test]
#[ignore = "host integration: Nix daemon, reviewed SLOPBOX_TEST_NODE, SLOPBOX_TEST_NIX and SLOPBOX_TEST_SLOPBOX"]
fn native_cli_project_nix_environment() {
    let output = std::process::Command::new(
        std::env::var_os("SLOPBOX_TEST_NODE").expect("reviewed Node path required"),
    )
    .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/native/nix.mjs"))
    .arg(std::env::var_os("SLOPBOX_TEST_SLOPBOX").expect("built Slopbox path required"))
    .arg(std::env::var_os("SLOPBOX_TEST_NIX").expect("reviewed host Nix path required"))
    .env_clear()
    .current_dir("/")
    .output()
    .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains(
        "native Nix builds, role separation, private cache and closure lifetimes passed"
    ));
}

#[test]
#[ignore = "host integration: installed Homebrew Git/ripgrep and reviewed SLOPBOX_TEST_* paths"]
fn native_cli_homebrew_tools() {
    let output = std::process::Command::new(
        std::env::var_os("SLOPBOX_TEST_NODE").expect("reviewed Node path required"),
    )
    .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/native/cli.mjs"))
    .arg(std::env::current_exe().unwrap())
    .arg(std::env::var_os("SLOPBOX_TEST_SLOPBOX").expect("built Slopbox path required"))
    .arg(std::env::var_os("SLOPBOX_TEST_PI_CLI").expect("reviewed Pi path required"))
    .args(["none", "live", "homebrew"])
    .env_clear()
    .current_dir("/")
    .output()
    .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout)
            .contains("Homebrew Git and ripgrep passed through the production supervisor")
    );
}

#[test]
#[ignore = "integration: set SLOPBOX_TEST_NODE, SLOPBOX_TEST_PI_CLI and SLOPBOX_TEST_SLOPBOX"]
fn native_cli_model_tool_round_trip() {
    let node = std::env::var_os("SLOPBOX_TEST_NODE").expect("reviewed Node path required");
    let cli = std::env::var_os("SLOPBOX_TEST_PI_CLI").expect("reviewed Pi path required");
    let slopbox = std::env::var_os("SLOPBOX_TEST_SLOPBOX").expect("built Slopbox path required");
    for (harness, workspace) in [
        ("none", "live"),
        ("data", "live"),
        ("trusted", "live"),
        ("trusted", "read-only"),
    ] {
        let output = std::process::Command::new(&node)
            .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/native/cli.mjs"))
            .arg(std::env::current_exe().unwrap())
            .arg(&slopbox)
            .arg(&cli)
            .args([harness, workspace])
            .env_clear()
            .current_dir("/")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            String::from_utf8_lossy(&output.stdout)
                .contains("native CLI model/tool round trip passed")
        );
    }
}
