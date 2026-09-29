# Native macOS

The experimental Apple-silicon backend uses Seatbelt with a host supervisor and explicitly selected native Mach-O/script resources. It shares the host kernel and gives ordinary subprocesses the selected command's full outer account/model authority; it has no generic restricted tool role. Historical macOS spike/handoff documents describe a removed embedded Pi adapter and should not be used as current configuration instructions. See [security model](../SECURITY-MODEL.md) and [selected native runtime](poc-native-runtime.md).

## Install and configure

The published Homebrew 0.2.0 formula in `pprotas/tap` installs the older released source; building this unreleased branch requires `cargo build --locked --release` or `nix build`. A logged-in GUI launchd domain and `/usr/bin/sandbox-exec` are required. Generic selected commands need host-owned configuration:

```toml
default_command = ["/Applications/Xcode.app/Contents/Developer/usr/bin/git", "status"]

[policy]
harness = "none"
credentials = "none"

[runtime]
executables = ["/Applications/Xcode.app/Contents/Developer/usr/bin/git"]
```

Choose actual executable paths, not version-manager shims, and explicitly grant additional interpreters, dynamic dependencies and application resources. Host `[runtime].dependency_roots` grants discovered dylibs only; `[runtime].bundles` grants complete validated read-only trees. A selected Nix store binary must have all necessary resources available through the selected runtime. The launcher never runs or evaluates the project's flake to infer a tool environment. A repository cannot grant host runtime resources, accounts or signing rights.

```bash
slopbox status --verbose
slopbox doctor
slopbox run --dev-env none -- /Applications/Xcode.app/Contents/Developer/usr/bin/git status
```

`status` is a configuration report, and `doctor` checks prerequisites; neither proves the selected tool will run. Project staging, `runtime=project`, implicit Nix activation, automatic harness/tool separation and generic named application IPC are not implemented on this backend. Host approval view is opt-in via `--approval-view` on an interactive command. Attached accounts, HTTPS mediation, secret commands and Git SSH signing use host brokers; subprocesses retain their authority. Test disposable credentials and selected resources before attaching real accounts. The production macOS package does not embed Pi or any other harness.

## Development checks

```bash
cargo fmt --all -- --check
cargo test --locked --all-targets
cargo clippy --locked --all-targets -- -D warnings
nixfmt --check flake.nix tests/nixos.nix
nix build
```

Host-only regression tests in `tests/native/cli.rs` are ignored by default. Set `SLOPBOX_TEST_SLOPBOX` to the built launcher and `SLOPBOX_TEST_NODE` to a reviewed Node binary before running the generic, account, signing and approval fixtures. The fixtures use disposable workspaces and synthetic credentials; real model integration tests are separately opt-in and must use credit-limited accounts.
