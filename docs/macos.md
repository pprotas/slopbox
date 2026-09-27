# Experimental native macOS launcher

The native path uses Slopbox's normal coordinator and gateway. It supports an explicitly selected Pi/Node runtime with separately sandboxed bash, or [generic selected executables](poc-native-runtime.md) without Pi configuration. Host-tool sessions need neither a VM nor Nix. This is not the complete Linux feature set or a cross-version support claim.

Tested on Apple silicon, macOS 27.0 build 26A428, Node 24.9.0 and Pi 0.85.1. A logged-in GUI launchd domain and `/usr/bin/sandbox-exec` are required. No installations or security-setting changes are performed automatically.

## Build

Apple Silicon macOS has an `aarch64-darwin` Nix package and development shell. Enable Nix's CLI/flake features for this host terminal without changing persistent settings:

```sh
export NIX_CONFIG="${NIX_CONFIG:-}
extra-experimental-features = nix-command flakes"
nix build
nix develop --command cargo fmt --check
nix develop --command cargo clippy --all-targets -- -D warnings
nix develop --command cargo test --locked
nix fmt -- --check flake.nix
nix flake check
```

Run these on the host, not inside Slopbox. `result/bin/slopbox` is the packaged executable; it has no Linux wrapper or dependency on Bubblewrap/Wayland. The shell supplies Rust, Cargo, Clippy, rustfmt, Git and nixfmt. Nix's Darwin stdenv supplies the compiler and SDK.

`nix build` and `nix flake check` run the package tests plus installed `--version`/`--help` checks. Darwin packages also reject non-system dylib dependencies, so copied workers do not need Nix-store read grants. This install check uses the host's `/usr/bin/otool`. The Darwin build skips the coalition, system-diff, private-PTY and set-id-file tests; the host `cargo test` command above runs them. Build tests allow loopback for disposable HTTP upstreams, not Internet access. Ignored launchd/Seatbelt/PTY integration tests remain separate host checks below. The `e2e` app remains Linux-only.

This packages Slopbox, not its selected Pi/Node runtime. Keep the explicit host runtime configuration below. Building or entering a host Nix shell does not import that shell into Slopbox; project activation is selected separately at launch.

Pawel confirmed the initial `nix build -L` and `result/bin/slopbox run --approval-view --dev-env none -- pi --provider openai-codex --continue` from the Nix/direnv shell without unsetting `DEVELOPER_DIR`. A subsequent signed-commit attempt exposed an unused Nix-store `libiconv` dependency in the copied worker. After the linker fix and switch to native `otool`, Pawel reports the rebuilt package, tests and install check passing. The agent also verified that the new session's signing worker has only system-library dependencies and passes the dependency check. The separate enforcement/PTY suites have not been rerun against this package.

Without Nix, use the installed Rust toolchain:

```sh
cargo build --locked
```

## Generic commands

Host-owned `[runtime].executables` enables ordinary command launch with `harness=none`. This bounded slice accepts native Mach-O/system-library executables and simple shell scripts, not application bundles or arbitrary dylib discovery. It preserves the native supervisor and Seatbelt boundary; subprocesses share outer authority rather than acquiring Pi's tool separation. See [configuration and enforcement tests](poc-native-runtime.md) and [native Claude Code acceptance](poc-claude-code.md).

## Run Pi

Add the selected runtime to the **host** `~/.config/slopbox/config.toml` (or `$XDG_CONFIG_HOME/slopbox/config.toml`). Use the actual Node executable, not a version-manager shim, and Pi's installed `dist/cli.js`:

```toml
[macos]
node = "/absolute/path/to/node"
pi_cli = "/absolute/path/to/pi-package/dist/cli.js"
# Optional: per-tool ceiling, 1–3600 seconds; default 120.
# tool_timeout_seconds = 120
```

From the project directory, using the built executable's absolute path:

```sh
/path/to/slopbox run --dev-env none -- pi
```

Pi options follow `pi`, for example `--provider openrouter --model openai/gpt-4o`. Use Slopbox's host-side model credentials, not Pi's host auth files. OpenRouter reads `OPENROUTER_API_KEY` from the host environment; the guest receives only a synthetic marker. No real provider account was used in the native tests.

The default developer policy uses a live workspace. Existing host/project policies and saved setup ceilings still apply. Native launch accepts `runtime=host` or `runtime=project` and a live or read-only workspace. The developer default is `harness=trusted`; `harness=data` imports data without extensions, and `--no-host-pi-resources` (or `harness=none`) disables imports. Remove any earlier `harness=none` override if you want trusted imports. A repository cannot select the runtime or expand access.

## Project Nix environments

`--dev-env auto` (the default) selects the default development shell when `flake.nix` exists. `--dev-env flake` requires one; `--dev-env none` keeps host-tool discovery without project activation:

```sh
/path/to/slopbox run --dev-env flake -- pi
```

Review the flake and its inputs before launching. Nix evaluation and realization run on the host using a packaged Nix executable and a cleared environment. This is trusted host preparation, not sandboxed Nix evaluation. Slopbox does not accept flake-supplied Nix configuration or update lockfiles. It reads `print-dev-env --json`; it never sources project activation on the host.

The supervisor activates the captured environment and runs `shellHook` before each tool request, **inside the tool Seatbelt role**. Selected Nix tools precede the base PATH. Compiler/SDK settings come from the dev shell, while homes, temporary paths, Cargo cache, broker settings and Git configuration remain sandbox-owned. Pi does not receive the project environment or its closure grants. Hooks may use the authority already granted to tools, including configured account routes.

With `runtime=host`, recognized host tools remain available as fallback. Host policy `runtime=project` requires a dev shell and omits Homebrew/Rustup/mise/Apple development installations; only the selected closure, the existing system base and explicitly configured Node remain available to tools. It does not enable staged workspaces or the contained profile.

Neither mode grants the whole Nix store, host profiles or daemon socket. The session-owned Nix profile pins the closure until native cleanup succeeds; uncertain recovery retains it. Changes to the flake require a fresh Slopbox launch. Structured-attribute development environments are currently rejected explicitly.

Local preparation, activation, profile and CLI tests pass. Pawel reports Nix formatting, the package build and the [host fixture](#project-nix-host-check) passing, including compilation/linking, hook confinement, role separation, cache reuse and concurrent-session cleanup. A fresh actual Pi session independently selected Nix Rust 1.98.1, Clang 21.1.8 and SDK 14.4. Inside it, 32 focused tests, both crates' formatting/strict Clippy, Nix formatting and brokered repository reads with Nix Git 2.55.0 passed. Forgejo PR and main Linux Rust/package/E2E CI subsequently passed for this change.

## Boundaries and limits

- Pi's built-in read/write/edit tools run inside the **Pi harness sandbox**, as on Linux. They can access the permitted workspace and Pi's sandbox-owned state, not arbitrary host files. They retain Pi's normal schemas, editing, images and pagination; there is no custom file-tool implementation. Host `defaultTools` settings and supported tool-selection CLI flags still apply.
- The Slopbox bash adapter routes interactive `!`/`!!` and model-called bash through the host supervisor, with no local-execution fallback. Bash/project processes get a separate home and no model endpoint or harness state.
- Without a project environment, developer tools are selected from recognized installations: Homebrew Cellar, activated mise versions, an installed Rust toolchain, and selected Apple compiler/SDK directories. Arbitrary PATH entries and whole Homebrew prefixes do **not** become read grants. Validated `opt/<formula>` links into installed Cellar kegs get literal read access and ancestor metadata only; `opt`, `etc` and `var` are not recursively granted. Rust runs installed cargo/rustc directly, with a private persistent Cargo cache, not the host Cargo configuration. CC, CXX, SDKROOT and Cargo's target linker select the installed Apple tools. This is not a full Xcode compatibility claim.
- Bash requests remain limited to 16 KiB and combined output to 32 KiB. These transport limits do **not** apply to Pi's built-in file tools. Bash output is returned when the command finishes, not streamed; cancellation reaps descendants.
- Host resources use the **same discovery and settings filter as Linux**: conventional extensions (trusted mode only), skills, prompts, themes, and supported preinstalled npm package resources. Selected roots are read-only in the harness, with native aliases for conventional Pi paths and configured read-only mounts. Host AGENTS.md and allowlisted settings are imported; host auth.json, models.json, raw settings and host session history are not exposed. This is the existing supported subset, not a copy of the entire host Pi directory.
- Trusted extensions execute with harness authority, as on Linux; they are not untrusted project code. Extensions needing other executables/services can still encounter native sandbox restrictions. No extra authority is granted just because a plugin requests it. Package installation remains a host operation; unsupported package-source/filter behavior remains that of the shared importer.
- Configured Git identities and URL rewrites use the shared signing/account brokers. Pawel reports the disposable native host fixture passing; live SOPS-backed repository reads and a real broker-signed checkpoint now pass too. Only the generated Git configuration, signing helper and its specific socket are exposed—not host Git configuration, tokens or the SSH agent. Account routes remain available with general/model networking disabled.
- Project extension/settings loading remains disabled; ordinary AGENTS.md context loading is restored. Temporary-overlay Pi mounts, staged workspaces, clipboard integration, and dry-run are not enabled. Unsupported capabilities fail explicitly.
- Opt in to the shared host approval view with `slopbox run --approval-view --dev-env none -- pi`. Ctrl-] opens it; `q` then Enter returns to Pi. Session/project approvals and revocations require fresh confirmation codes and never replay requests. The view requires a foreground host TTY on all three streams and at least 80×24 for mutations. The separate host `slopbox network events`, `approve`, and `revoke` commands remain available. General egress remains deny-by-default; the native adapter does not yet append denial IDs to tool output.
- Pi 0.85.1 uses **Escape** to cancel bash and **Ctrl+D** to exit. Resize, suspension/resume, normal exit, SIGTERM and SIGHUP restoration are tested.

Temporary launchd jobs own process coalitions and broker-port leases. Stale native sessions are recovered before the next launch; invalid ownership records retain authority reservations and require inspection. Do not delete retained journals or unload their jobs to bypass a recovery error. Power-loss/logout recovery and every startup crash window are not yet validated.

## Troubleshooting

A Nix-built Git helper failing to load `/nix/store/.../libiconv.2.dylib` exposed an unused link dependency. Darwin package and dev-shell builds now strip unused dylibs; the package rejects remaining non-system libraries. Rebuild and start a fresh Slopbox session—the running worker is an immutable copy. Do not add store grants or disable signing.

`invalid Apple tool clang` from a Nix/direnv shell was caused by its SDK-only `DEVELOPER_DIR`. Native host discovery now ignores values under `/nix/store` and uses the system-selected Xcode/Command Line Tools installation. Other overrides remain validated. Keep the shell's SDK variables for Nix builds; rebuild Slopbox rather than unsetting them in `.envrc`. Ambient SDK variables do not grant store access; a selected project environment supplies its own SDK and closure. The host-selection regressions pass locally, and the packaged host launch is confirmed above.

A dyld error for `/opt/homebrew/opt/...` saying `blocked by sandbox` is not a missing package. Cellar access alone omitted the dependency symlinks used by Homebrew install names. The fix adds narrowly validated link access. Its host enforcement and Pi integration tests passed, and Git/ripgrep now work in the restarted session. Discovery is a Slopbox-startup snapshot: newly installed packages can introduce dependency links absent from an already-running session. Finish configuration before relaunching Slopbox; restarting Pi alone does not refresh grants. Do not reinstall packages or grant the whole Homebrew prefix to work around it.

Rustup shorthands such as `RUSTUP_TOOLCHAIN=1.91.0` resolve to an installed host-qualified directory, using rustup's default host triple or the native architecture. Older launchers incorrectly treated the shorthand as a literal directory name and reported an installed toolchain as missing; rebuild rather than reinstalling Rust. Fully qualified names from `rustup toolchain list` are also accepted. Resolution never installs a toolchain or silently chooses another version.

`fetch failed` is a transport error, not proof of an approval denial. Codex uses the fixed model broker; `slopbox network events` reports general-proxy denials and prints nothing when none are recorded. Do not add general egress grants to repair model transport.

A Darwin socket-mode race causing intermittent model failures was fixed after the first real Codex run. Exit Pi completely and restart the rebuilt launcher. If failures persist, include the broker diagnostics printed after Pi exits; do not share credentials or authorization headers.

The missing `fd`/`ripgrep` warnings describe unexposed helpers. Offline mode deliberately prevents automatic downloads; those warnings do not explain model fetch failures.

Git's automatic background maintenance can report `setsid failed: Operation not permitted` after a successful fetch. Use `git fetch --no-auto-maintenance origin main`, or command-local `git -c maintenance.auto=false …`, without granting daemonization or changing persistent Git configuration.

## Continue development in a new Pi session

The current work, uncommitted changes, constraints and remaining validation are recorded in [macos-handoff.md](macos-handoff.md). Context-file loading is enabled again. To explicitly load the handoff after launch, paste:

> Read `docs/macos-handoff.md`. Continue from “Goal and next task,” preserve the existing working tree, and report any host-only steps rather than trying to bypass the sandbox.

## Integration tests

Run from an authorized host terminal, after rebuilding; no account login is needed:

```sh
cargo build --locked
export SLOPBOX_TEST_SLOPBOX="$PWD/target/debug/slopbox"
export SLOPBOX_TEST_NODE=/absolute/path/to/node
export SLOPBOX_TEST_PI_CLI=/absolute/path/to/pi-package/dist/cli.js
cargo test --locked native_cli_tests::native_cli_model_tool_round_trip -- --exact --ignored
cargo test --locked native_cli_tests::native_terminal_round_trip -- --exact --ignored
```

To validate the Nix-installed launcher, build it with `nix build`, set `SLOPBOX_TEST_SLOPBOX="$PWD/result/bin/slopbox"` instead, and run these tests through `nix develop --command cargo test …`. Also run `native_cli_tests::native_approval_view_round_trip -- --exact --ignored` against that executable. Keep the reviewed Node/Pi paths; these tests use disposable host configuration, not real accounts. `otool -L result/bin/slopbox` reports the installed launcher's dynamic dependencies; a successful package build alone does not prove its copied worker runs under the native profiles.

The model test runs the normal CLI dispatch/coordinator, gateway, production Pi adapter and profiles. Only the provider's upstream destination is replaced by a loopback fixture in the **test binary**; production builds contain no upstream override. The expanded test covers none/data/trusted imports and a read-only workspace: upstream Pi read/write/edit (including a write larger than the bash request limit), an imported extension and npm dependency, permission-error credential/symlink/read-only denials, and unchanged RPC bash, general-egress denial, model/state isolation, cancellation and cleanup. The expanded none/data/trusted/read-only matrix has passed host validation with fake upstreams. This does not establish compatibility with every real host plugin. The terminal test launches the actual Slopbox executable in a PTY.

### Project Nix host check

After reviewing the fixture, run from the host with the reviewed Node path above:

```sh
nix fmt -- --check flake.nix tests/native/nix/flake.nix
nix build -L
export SLOPBOX_TEST_SLOPBOX="$PWD/result/bin/slopbox"
export SLOPBOX_TEST_NIX="$(command -v nix)"
nix develop --command cargo test --locked native_cli_tests::native_cli_project_nix_environment -- --exact --ignored
```

This uses a disposable probe in place of Pi, the production launcher/supervisor/profiles, a locked dev shell and no accounts. It checks Rust/C/C++ compilation and Nix zlib linking, sandboxed hooks, harness/closure separation, denied unrelated store/configuration/daemon access, private Cargo cache reuse, two concurrent profile roots and independent cleanup. Nix may realize fixture dependencies. Failures retain the disposable fixture and report its path; do not delete native recovery records to work around a failure.

After it passes, test actual Pi using `result/bin/slopbox run --approval-view --dev-env flake -- pi`. The existing model/tool and PTY fixtures remain separate checks.

### Host approval view

With the same `SLOPBOX_TEST_*` exports, run from a host terminal:

```sh
cargo build --locked &&
cargo test --locked native_cli_tests::native_approval_view_round_trip -- --exact --ignored &&
cargo test --locked native_cli_tests::native_terminal_round_trip -- --exact --ignored
```

The approval test uses a deterministic terminal probe under the production native profiles to check hostile output, input isolation, fresh confirmation, session/project scope, revocation, no replay, literal Ctrl-] forwarding, disabled networking, resize, suspension and failure restoration. It uses a reserved `.invalid` hostname and no real account. The terminal test uses actual Pi and checks opening/return redraw, shell execution/cancellation, and SIGTERM/SIGHUP restoration with the view open. Pawel reports both host tests passing and confirms Ctrl-] opens the view in a fresh `--approval-view` session. Visual behavior across other terminal emulators still requires separate feedback.

See [enforcement evidence](macos-spike.md) for the shared engine's conformance and crash-recovery tests. Keep Linux build, package and E2E CI green for changes to shared code.

Host validation before the opt-link change: 95 unit tests and four native CLI tests pass, including 18 Pi preparation/discovery and 10 runtime-discovery tests. Formatting, strict all-target Clippy, JavaScript syntax, the expanded model/tool integration matrix and the actual-CLI PTY test pass. A fresh native Pi/RPC session with `RUSTUP_TOOLCHAIN=1.91.0` also ran the installed Cargo and rustc 1.91.0 successfully. These checks used disposable configuration and no real model account.

### Homebrew opt-link regression

The Git/ripgrep smoke test reproduced the dyld denial before the fix. The patch passes 13 runtime tests, 18 Pi tests, formatting, strict all-target Clippy and JS syntax checks. Pawel ran both host tests below successfully. After restart, the agent independently verified Git 2.53.0, ripgrep 15.1.0, `git status`, and the real Git-init/status/ripgrep smoke in `tests/native/homebrew.mjs`.

With the reviewed `SLOPBOX_TEST_NODE` and `SLOPBOX_TEST_PI_CLI` exports above, run in a host terminal before restarting:

```sh
cargo test --locked backend::macos::runtime::tests::homebrew::opt_dependency_loading_and_denials -- --exact --ignored &&
cargo build --locked &&
SLOPBOX_TEST_SLOPBOX="$PWD/target/debug/slopbox" cargo test --locked native_cli_tests::native_cli_homebrew_tools -- --exact --ignored
```

The first test builds a disposable dylib/consumer with an `opt` install name and uses the production Seatbelt renderer. It checks read-only installation access, denied credential/config/state reads, symlink escape and post-application retargeting, and harness/tool separation. The second requires installed Homebrew Git/ripgrep and exercises their real operations through Pi and the production supervisor. It also runs the existing model/file-tool/broker/cancellation checks. Neither test changes the host Homebrew installation or uses a real model account. Restart only after both pass.

### Git signing and account routes

With the same three `SLOPBOX_TEST_*` exports, run:

```sh
cargo build --locked &&
cargo test --locked native_cli_tests::native_cli_git_signing_and_routes -- --exact --ignored
```

This creates a disposable SSH agent/key, bare Git repository and HTTP upstream. It exercises signed commits, fetch/pull/push, API authentication/redaction, method denials, wrong-identity/tag rejection, raw-agent/key denials, read-only generated files and cleanup through Pi's supervisor. A second session tests URL rewrites without a signing identity. The upstream override exists only in the test binary; no real account or repository is used. Signature verification runs on the host.

Pawel reports `cargo build --locked`, the default `cargo test --locked` suite and this host fixture passing. That does not cover the other ignored enforcement/PTY tests or Linux CI for this revision.

Native setup uses the shared [account routes and Git identities](configuration.md#authenticated-http-routes). Secrets may come from host commands, environment variables or SOPS. Credential helpers must resolve to recognized Homebrew Cellar/Nix-store executables outside the workspace. Earlier actual Pi sessions validated SOPS-backed Forgejo reads and broker-signed publication as `pi`. A later live session validated GitHub REST and Git smart-HTTP reads using the host `gh` credential command, plus brokered signing in a disposable repository. Guest OpenSSH verification fails at UID lookup; verify signatures on the host or through the forge.

[github.toml](github.toml) is a generic template, not this project's live configuration. Replace its placeholders and merge it into host configuration without replacing unrelated runtime settings. Stock `gh` uses a private account socket and synthetic tool-only authentication; the host `gh` login remains outside both roles. GraphQL requires its own explicitly approved capability; the template grants only repository-prefix REST access.

Pawel reports the GitHub host fixture passing after the worker-selection, request-lifetime and config-schema corrections. It uses stock `gh`, a disposable host credential store and a mock upstream—not a real GitHub account. A separate local check with real `gh` confirms the generated configuration needs no migration or writes.

To rerun the native fixture:

```bash
SLOPBOX_TEST_NODE=/absolute/path/to/reviewed/node \
SLOPBOX_TEST_GH=/absolute/path/to/reviewed/gh \
SLOPBOX_TEST_SLOPBOX="$PWD/result/bin/slopbox" \
nix develop --command cargo test --locked \
  native_cli_tests::native_cli_github_account -- --exact --ignored
```

It checks actual `gh` REST/GraphQL requests, host command lookup, reflected-token redaction, method/origin denials, read-only generated configuration and harness/tool environment separation. The upstream override exists only in the test binary for reserved fixture hostnames.

Homebrew `fj` 0.6.0 successfully reads repository and PR information through the broker without a local login token:

```sh
fj -C "$HOME" -H "${SLOPBOX_AUTHENTICATED_HTTP_BASE_URL:?account routes not configured}" repo view pi-and-i/slopbox
fj -C "$HOME" -H "${SLOPBOX_AUTHENTICATED_HTTP_BASE_URL:?account routes not configured}" pr search --repo pi-and-i/slopbox
```

`fj whoami` requires `read:user`, which this repository token lacks. Do not broaden its scope just for a health check or import a host login store. Restarting alone does not configure account routes or a signing identity.
