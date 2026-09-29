# Native macOS session handoff

> Historical design/validation record. Built-in Pi launch and integration have since been removed; use [current configuration](configuration.md) and the [security model](../SECURITY-MODEL.md) for supported behavior.

## GitHub cutover checkpoint — 2026-09-26, before publication

Cutover started from freshly fetched main at `0322733fc6e84e0f8fecbe26cb756dcfef88eda9`, after #12/PR #35 and its Linux CI passed. Pawel confirmed the Forgejo push mirror is disabled, the GitHub account is `pprotas`, and the repository must remain private. No license change was requested.

Live GitHub REST, Git smart HTTP and brokered commit signing work with the unchanged `pi <pi@pawelprotas.com>` identity. The signing key was matched against GitHub's registered keys; verifying the existing email resolved GitHub's initial `no_user` result. Tool authentication is synthetic and no raw SSH agent is exposed. Guest OpenSSH verification lacks UID lookup; do not widen grants for it.

Candidate `77bb9cc` is GitHub-verified and passed both hosted Rust/package jobs (Ubuntu 24.04 and Apple Silicon macOS 27). Host-exported `nix flake check --no-build --all-systems` output also passed. Earlier CI failures exposed interrupted HTTP reads and tests assuming `/bin/bash` exists in a Nix build sandbox; both were corrected without skips. The VM job then reached driver type checking, which caught a collision with its built-in `log` variable. The fixture now uses `e2e_output`; type checking and enforcement assertions remain enabled. VM execution still requires validation on the amended candidate.

Pawel separately reported the native GitHub fixture passing with real `gh`, disposable credentials, REST/GraphQL, redaction and role-denial checks. The real-CLI read-only configuration regression demonstrated failure before adding schema version 1 and success afterward. Hosted Rust/package checks do not replace ignored native enforcement and PTY fixtures.

Removed `.sops.yaml` and `secrets.yaml` only after live configuration stopped referencing them. The preliminary audit covered 98 tracked working files and 288 reachable historical objects; all six matches were disposable/synthetic markers or function calls. The historical credential field is SOPS ciphertext. This is neither proof of absence nor history erasure; the Forgejo archive and old refs remain.

Publication requires a single signed root with a valid GitHub signature and all three CI jobs green. Recheck remote main immediately before replacing it with an explicit force-with-lease. Commit-signing authorization does not cover tag signing. Issue migration and release-asset distribution remain separate. This checkpoint is historical; inspect GitHub refs, runs and releases for current publication status.

The remaining sections record earlier checkpoints; use [macos.md](macos.md) for current setup and validation commands.

## Earlier native checkpoints

Pawel wants to develop Slopbox using Pi **inside native macOS Slopbox**, not a Linux VM. The immediate goal is normal Pi inside the sandbox: built-in file tools, the same supported trusted configuration/plugins as Linux, and supervisor-routed bash—not a replacement Pi implementation.

The previous session spent too long expanding enforcement probes before connecting the public launcher. That connection is now implemented. **Do not restart the spike or make Xcode, more integrations, or a new framework prerequisites for using it.**

Pawel has confirmed that the rebuilt real Codex session works and terminal colors are normal after preserving `TERM`/`COLORTERM`. Continue development inside Slopbox, keeping an authorized host terminal for builds, tests and administration. The terminal-exit and post-cleanup port-rebind failures have targeted fixes and subsequent host passes. The restored native Pi tools/resources passed the expanded host integration tests, and the Rustup shorthand fix works in the restarted session. The Homebrew dependency-link fix also passed its host checks; Git/ripgrep work in the restarted session. Continue ordinary development inside Slopbox. Linux CI remains required before merging.

A matching gateway bug was reproduced with a 128 KiB fake model request: Darwin accepted Unix sockets inherited `O_NONBLOCK`, while HTTP handlers expected blocking reads. `src/gateway.rs` now explicitly makes accepted connections blocking for all three routes. The regression failed with `Resource temporarily unavailable` before the fix and passed three consecutive runs afterward. Exit the old Pi session completely before restarting; this is not fixed by network approvals.

- Host runtime configuration has now been created with Pawel's approval at `~/.config/slopbox/config.toml` (mode `0600`). `slopbox doctor --no-host-pi-resources` reports zero failed checks and an existing Codex credential as configured, **not validated**. No login or real model request was performed during setup.
- Pawel approved checkpoint commit/push/PR publication and the native Git broker port. The real account/signing configuration is active: repository reads through Git and `fj` pass, and the broker signed native checkpoint `da119be` as `pi`. No raw token, age identity or SSH agent is exported to tools.
- [PR #5](https://forgejo.home.pawelprotas.com/pi-and-i/slopbox/pulls/5) is published. Forgejo verifies both checkpoint signatures (`da119be`, `67a1501`). Linux run 15 failed Clippy because macOS-only closure bodies became no-ops on Linux. The follow-up gates entire native validation steps; six CLI and ten policy/session tests plus local formatting/Clippy pass. Linux CI must confirm the fix; consolidated native host validation remains pending. A locked KeePassXC can cause the signing broker to refuse commits; unlock the existing host agent key rather than disabling signing or restarting unnecessarily.
- Record actual results or the exact blocker in [macos-spike.md](macos-spike.md). Do not turn mock results into a claim of real-account validation.
- If a required tool is blocked, report the missing capability. Do not broaden filesystem/network access, expose credential roots, or add an unsandboxed fallback to get past it.

Repository and CI direction: see the [GitHub cutover plan](roadmap.md#repository-and-ci-hosting).

## Original repository checkpoint

- Repository: `/Users/pawel/Projects/slopbox`.
- Branch: `feat/macos-seatbelt-spike`, based on `56e3cfa`; a fresh authenticated fetch confirms that is still `origin/main`.
- Initial build/probe checkpoint: `d8c2afd`. Native runtime, tools/resources, signing/SOPS and test checkpoint: `da119be`, signed as `pi <pi@pawelprotas.com>`.
- Inspect the current working tree before continuing; do not reset it, switch to trunk, or discard untracked files.
- Deleted probe `coalition.rs` and `supervisor.rs` were moved into `src/backend/macos/engine/`, not abandoned. The probe crate imports the shared implementation.
- Earlier refactors `35e0e63`, `292643e`, and `2fab1cb` were already merged before this branch.

Publication was approved for the native checkpoint only, not subsequent feature work, repository migration, software installation or broader host access. Do not change persistent Git configuration without authorization. Use the requested bot identity `pi <pi@pawelprotas.com>`, not Pawel's personal Git identity. The selected KeePassXC-agent key is `SHA256:RLZt+rEN3XFdMtf4jw0cf66dDia3VPrG75Gdl2SC2Ho`; native signing uses `/usr/bin/ssh-keygen`, not the 1Password signing wrapper. Never expose the agent socket or private key to the sandbox.

## What works, and what does not

The public launcher uses the existing coordinator, policy and gateway. Pi runs in a deny-default Seatbelt role; its bash adapter requests a separately sandboxed tool role from the host supervisor. Bash/project processes do not receive model credentials, model-endpoint access, or harness-state access. Pi built-in file tools intentionally run with harness filesystem authority, as on Linux. Generated configuration and homes are session-owned; project history persists separately.

Native preparation now enables upstream Pi read/write/edit without custom implementations, restores ordinary context loading, and uses the shared resource importer for none/data/trusted modes. Host auth and raw configuration remain excluded. Imported roots are read-only in the harness; project extension/settings loading remains disabled. The old mandatory `--no-host-pi-resources` restriction is removed. The Pi parity/Rustup host integration validation passed, and read/write/edit plus an offline Rust compile/run now work in the restarted session. The Homebrew opt-link patch subsequently passed its own host tests and the live-session Git/ripgrep smoke.

The runtime draft now selects bounded installed toolchains instead of arbitrary PATH roots or whole Homebrew prefixes. Cargo compilation and focused unit tests ran inside this sandbox using the already-authorized direct Apple compiler, archiver, SDK and linker. Full Xcode compatibility is not established. Native tools cannot bootstrap another Slopbox sandbox or administer the running one. Launcher rebuild/restart, launchd integration tests, login, approvals and recovery remain **host-terminal** operations. Workspace code changes do not update the running worker snapshot, adapter or profile; rebuilding and restarting is a host operation.

Bash requests are capped at 16 KiB, combined output at 32 KiB, and execution at the host-selected ceiling (default 120 seconds). Those bash transport limits do not apply to Pi's restored built-in file tools. Prefer `rg` when available; use Node for file inspection if it is not exposed rather than installing tools or importing the host environment.

Temporary-overlay Pi mounts, staging, project environment activation, clipboard integration, and dry-run remain disabled. Issue #16 enables the shared opt-in host approval view without additional guest grants. Pawel reports the [approval/real-Pi PTY cases](macos.md#host-approval-view) passing after the relay fix and the fixture's master-side termios check. Darwin can return `ENOTCONN` from write-side shutdown with unread reply bytes; the old relay lost responses in 14/50 live probes. The relay now preserves those bytes, with a regression that failed before the fix and passes in both crates. The signing/URL-rewrite port passed its disposable host fixture according to Pawel; live repository reads and a broker-signed checkpoint now pass too. Use host `slopbox network` commands for explicit approvals; failed operations are never automatically replayed.

## Host launch

Setup and supported flags: [macos.md](macos.md). Commands below are for Pawel's host terminal, not sandboxed bash. After configuring the runtime, authenticate if needed and start a new session:

```sh
./target/debug/slopbox auth login openai-codex
./target/debug/slopbox run --dev-env none -- \
  pi --provider openai-codex
```

The reviewed runtime paths on this machine were:

```toml
[macos]
node = "/Users/pawel/.local/share/mise/installs/node/24.9.0/bin/node"
pi_cli = "/opt/homebrew/Cellar/pi-coding-agent/0.85.1/libexec/lib/node_modules/@earendil-works/pi-coding-agent/dist/cli.js"
```

Merge this into host configuration only with authorization; do not replace existing configuration. Model credentials stay in Slopbox's host-side broker. Do not copy Pi's host authentication files into the workspace or guest. The native runtime uses bundled model metadata (`PI_OFFLINE=1`). Safe host settings/resources are imported according to the effective harness policy; an existing `harness=none` override or saved ceiling still disables imports.

Recorded host: Apple M1 Pro, macOS 27.0 build 26A428, Xcode 27.0, Rust 1.91.0, Node 24.9.0, Pi 0.85.1. GUI `launchd` domain required. The original spike did not use Nix; Pawel has since confirmed nix-darwin is installed. Native project-environment activation is still unsupported. Only the Darwin Rust target was observed installed. Do not install software, accept licenses, or change host security settings without approval.

## Why the backends differ

Linux delegates filesystem/process/network isolation to bubblewrap and kernel namespaces. Seatbelt restricts access without providing the same private environments:

- Nested `sandbox_apply` fails on this host (exit 71). Harness and tools therefore launch independently with different profiles, not as nested sandboxes.
- Native roles share the host network namespace. Each session needs distinct IPv4 broker ports, held by launchd until all authorized tasks are gone. Port secrecy is not enforcement.
- Process groups failed detached-descendant cleanup. Launchd-created resource coalitions provide ownership; version-checked signals and kernel task counts establish cleanup. Journals allow fresh-process recovery.

These are backend mechanisms, not a reason to duplicate policy, session orchestration, provider authentication or the gateway. Keep shared behavior shared. Do not replace tested ownership with process-group-only cleanup or release leases when task ownership is uncertain. Invalid journals are retained for inspection, not deleted to bypass an error.

## Implementation map

| Area | Files |
|---|---|
| Normal orchestration and dispatch | `src/session.rs`, `src/main.rs`, `src/backend/mod.rs` |
| Native configuration, profiles and launch | `src/backend/macos.rs`, `src/backend/macos/profile.rs` |
| Ownership, supervision, leases, stdio and recovery | `src/backend/macos/engine/` |
| Native Pi preparation and bash routing | `src/harness/pi/native.rs`, `assets/pi-extension.ts` |
| Shared gateway and model providers | `src/gateway.rs`, `src/provider/` |
| Terminal relay | `src/terminal.rs` — Darwin uses `select`, Linux retains `poll` |
| Public-path tests | `tests/macos-cli.rs`, `tests/native/` |
| Shared-engine enforcement and crash tests | `tests/macos-seatbelt/` |

## Current change: Pi parity and Rustup resolution

- Removed bash-only/builtin/context restrictions in `src/harness/pi/native.rs`; did not add custom read/write/edit implementations to `assets/pi-extension.ts`.
- Reused discovery, settings filtering and resource-argument generation from `src/harness/pi.rs`. Native adaptation grants canonical selected roots to the harness and provides conventional Pi paths in its private home. Private-state/credential ancestors and aliases are rejected; temporary overlays fail explicitly.
- Expanded `tests/native/cli.mjs` and its Rust runner for none/data/trusted imports plus a read-only workspace. It requests a >16 KiB write, exact edit, paginated read, imported extension/npm dependency execution, and permission-error denials for host auth, symlink escapes and read-only resources. Bash model/state isolation and cancellation checks remain.
- Host validation now passes: 95 unit tests + four native CLI tests, the expanded none/data/trusted/read-only integration matrix, the actual-CLI PTY test, formatting, strict all-target Clippy, JS syntax and diff checks. This includes 18 Pi tests and 10 runtime-discovery tests. Integration tests use fake upstreams; real host-plugin compatibility still needs confirmation.
- Fixed the false missing-toolchain error for `RUSTUP_TOOLCHAIN=1.91.0`: Rustup stores it as `1.91.0-aarch64-apple-darwin`. Shorthand lookup uses the configured default host triple or native architecture, without changing the selected version or exporting host configuration. The regression failed before the fix. A fresh native Pi/RPC session then ran the installed Cargo/rustc 1.91.0 successfully. The launcher is rebuilt; no installations or host configuration changes were made.
- Previous custom file-tool transport work was abandoned without being applied. Keep upstream Pi behavior; do not restart that design.

## Resolved: Homebrew opt links

- The restarted session runs Cargo/rustc 1.91.0 and upstream read/write/edit. A dependency-free offline Cargo compile/run passed without compiler-environment overrides.
- Before the fix, Homebrew Git and ripgrep aborted on `/opt/homebrew/opt/pcre2/lib/libpcre2-8.0.dylib`. Direct Cellar reads succeeded; opt-link reads and metadata returned EPERM. `tests/native/homebrew.mjs` reproduced the failure.
- The patch adds literal read grants only for symlinks to canonical `Cellar/<formula>/<version>` directories, plus ancestor metadata. It does not grant the opt tree or the Homebrew prefix. Invalid/dangling/escaping links and regular opt entries do not add grants; a symlinked opt directory is refused.
- New tests cover discovery, rendered role separation, and a host-only production-profile dylib/denial/retargeting fixture. `native_cli_tests::native_cli_homebrew_tools` checks actual Homebrew Git/ripgrep through Pi and the production supervisor. Pawel ran both host-only tests successfully, then restarted Pi. The agent independently confirmed Git 2.53.0, ripgrep 15.1.0 and the real Git-init/status/ripgrep smoke inside that restarted session.
- Local validation: 13 runtime and 18 Pi tests, formatting, strict all-target Clippy and JS syntax pass. The Pi preparation fixture now creates its own dummy executable instead of depending on `/usr/bin/true` canonicalization in the current restricted environment. No production grant was added for that test dependency.
- Reproduction commands remain in [macos.md](macos.md#homebrew-opt-link-regression). No Rust/Homebrew reinstall, host configuration change or whole-prefix grant was needed. Git status now works; the branch remains `feat/macos-seatbelt-spike` with the existing uncommitted work.

## Review fixes before merge

Native Pi resource checks now precede secret resolution and project-state creation. `doctor` and `status` report unsupported overlays and credential aliases consistently; a CLI regression covers both. The CLI fixture uses its own executable instead of `/usr/bin/true`. Native-session tests now use the existing bounded post-cleanup port-rebind retry; live-lease assertions remain immediate. No sandbox grants or recovery policy changed.

Local validation: 13 runtime tests, 18 Pi tests and five CLI tests pass. Both crates pass formatting and strict all-target Clippy; probe tests compile, and JS/TS syntax and diff checks pass. Launchd/PTY integration and Linux CI have **not** been rerun for this patch. The original unsigned-Xcode milestone remains open; this is an experimental Pi/native-tool checkpoint.

## Native Git broker port

Native sessions now use the shared workspace-bound identity and URL-rewrite configuration. Rewrites use the account port after launchd leases it. The signing helper executes the immutable worker snapshot and connects only to its unique signing socket; the host agent and private key remain outside both roles. Native signing uses `/usr/bin/ssh-add` and `/usr/bin/ssh-keygen`, not PATH wrappers. Accepted signing streams now reset Darwin's inherited nonblocking mode, and duplicate author/committer headers are rejected.

Local checks pass: 47 distinct Git/profile/runtime/Pi/CLI tests, formatting, strict all-target Clippy and JS syntax. Pawel subsequently reported `cargo build --locked`, the default `cargo test --locked` suite and `native_cli_tests::native_cli_git_signing_and_routes` all passing in the host terminal. The fixture uses disposable credentials and real Git smart HTTP through Pi, then verifies the signature on the host; see [macos.md](macos.md#git-signing-and-account-routes). This is user-reported host evidence, not real Forgejo validation. Other ignored enforcement/PTY suites and Linux CI remain pending for this revision.

The historical Forgejo host configuration was active in that relaunched session: `pi <pi@pawelprotas.com>`, the selected KeePassXC key, SOPS secret `scm_pi` and four fixed Forgejo routes. The generated public key hashes to the requested fingerprint, signing is enabled, and commit `da119be` contains an SSH signature from the real broker. Raw agent, token and age-key environment variables remain absent from tools. Guest signature verification is not enabled; verify on the host or through Forgejo.

Pawel installed Homebrew SOPS and corrected the Mac's age identity to match NixOS. The real launch successfully resolved the SOPS-backed routes. Native helper selection accepts executable Cellar packages or Nix-store files outside the workspace; Linux lookup and the cleared helper environment are unchanged. Latest local checks pass: 52 distinct Git/profile/runtime/Pi/SOPS/CLI tests, both crates' formatting and strict Clippy, probe compilation and JS/TS syntax. A full sandboxed run still hits long Unix-socket paths and denied system-helper/PTY operations; consolidated host validation remains required.

Homebrew `fj` 0.6.0 now runs. Repository view and open-PR listing through the `api` broker route succeed without a local login token. `whoami` reaches Forgejo but the token lacks `read:user`; do not broaden scopes just for that check. Use `fj -C "$HOME" -H "$SLOPBOX_AUTHENTICATED_HTTP_BASE_URL"` with explicit repository arguments. Keep the host login store private.

Git `ls-remote` and fetch use the authenticated smart-HTTP rewrite; repository remotes remain unchanged. Automatic background maintenance attempted a denied `setsid` after a successful fetch. `git fetch --no-auto-maintenance origin main` succeeds cleanly; commits use the command-local `-c maintenance.auto=false`. No persistent Git settings or sandbox grants were changed.

## Earlier validation (before these changes)

- Main crate: 81 unit tests + four native CLI tests passed; build, formatting and strict Clippy passed.
- Shared-engine suite: 35 default tests + three opt-in Pi tests passed, including concurrent sessions and crash recovery with actual Slopbox workers.
- Two normal-path integration tests passed three consecutive rounds: model/tool/model through the real coordinator/gateway with a test-only upstream override, and the actual CLI in a PTY.
- PTY coverage: literal `!`/`!!`, Escape cancellation, Ctrl+D exit, resize, suspension/resume, restoration after normal exit/SIGTERM/SIGHUP, and truecolor/256-color detection. The color regression failed before the fix and passed afterward. Pi 0.85.1 uses Escape to cancel bash; a single Ctrl+C clears the editor.
- JavaScript/TypeScript syntax and `git diff --check` passed. No native/probe launchd jobs or native session directories remained after cleanup.
- A later concurrent validation run hit a terminal-test exit timeout and `AddrInUse` in `crash_recovery_does_not_stop_a_second_live_session` after cleanup. Subsequent targeted fixes use Ctrl+D for Pi exit and a bounded retry only for post-cleanup port rebind. Pawel then validated the terminal test, individual recovery test and default Seatbelt suite (33 passed, 4 ignored). Those passes predate the current runtime/resource changes.
- The explicitly ignored nested-profile test still fails; separate-role execution avoids that requirement. Do not report every ignored test as passing.
- **Linux `rust`, `package`, and `e2e` CI has not run for these changes.** Codex success is user-reported; automated native tests still use fake upstreams. Xcode, cross-version behavior, logout/power loss and every startup crash window remain unvalidated.

Reproduction commands are in [macos.md](macos.md#integration-tests) and [macos-spike.md](macos-spike.md#reproduce). Ask for host-side runs when tools are unavailable inside the sandbox; do not claim tests ran there. Before changing code, read [architecture.md](architecture.md) and relevant parts of [the security model](../SECURITY-MODEL.md), whose detailed namespace/mount behavior is Linux-specific.

Keep updates concise. Fix concrete blockers to the usable native path, retain negative evidence, and preserve the Linux implementation and credential/approval boundaries. Broader features are separate follow-up work.
