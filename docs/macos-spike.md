# macOS Seatbelt spike

Recorded 2026-09-20 on `feat/macos-seatbelt-spike`, branched from freshly fetched `origin/main` at `56e3cfa`. The three refactor checkpoints in [macos-handoff.md](macos-handoff.md) are merged.

**A narrow experimental native launcher now works.** The normal coordinator and gateway run an explicitly selected Pi/Node runtime with separately sandboxed bash. See [setup and limits](macos.md). Production-profile model/tool integration and actual-CLI terminal tests pass with disposable credentials. The shared engine separately passes concurrent-session and fresh-process crash recovery tests. Xcode and broader feature parity remain incomplete; nested profiles still fail.

The lifecycle implementation lives in `src/backend/macos/engine/`; `tests/macos-seatbelt/` compiles that same source with only `libc`. The Linux CI configuration is unchanged, but Linux checks still need to run. Temporary `launchd` jobs are created and removed by the probes; no persistent agent is installed. Ordinary tool/harness jobs are one-shot; socket-lease jobs must retain demand activation, as explained below. No software installation, license acceptance, account login, TCC/admin grant, or host security configuration change was needed.

## SCM configuration follow-up

The historical Forgejo bot configuration was validated after relaunch; live configuration belongs outside the repository. Host SOPS resolves the account secret, the generated public signing key matches the selected fingerprint, and checkpoint `da119be` was signed through the real broker as `pi`. Git `ls-remote`/fetch and `fj` repository/PR reads succeed. `whoami` is denied for missing `read:user`; repository operations do not require expanding that scope. Raw token, age-key and agent environment variables remain absent from tools.

Latest local validation passes 52 targeted tests, both crates' formatting and strict Clippy, probe compilation and JS/TS syntax. Consolidated host validation and Linux CI remain required. The earlier sandboxed full-suite attempt timed out with socket-path-length, native PTY and system-helper failures; it is not a pass. Git's optional detached maintenance is also blocked; command-local suppression avoids the `setsid` error without widening grants. See [the handoff](macos-handoff.md#native-git-broker-port).

## Host inventory

All commands below exited 0.

| Command | Output |
|---|---|
| `sw_vers` | macOS 27.0, build 26A428 |
| `uname -m` | arm64 |
| `sysctl -n machdep.cpu.brand_string` | Apple M1 Pro |
| `xcode-select -p` | `/Applications/Xcode.app/Contents/Developer` |
| `xcodebuild -version` | Xcode 27.0, build 27A266a |
| `xcodebuild -checkFirstLaunchStatus` | No output; first-launch prerequisites satisfied |
| `xcrun --find clang` | `/Applications/Xcode.app/Contents/Developer/Toolchains/XcodeDefault.xctoolchain/usr/bin/clang` |
| `xcrun --sdk macosx --show-sdk-path` | `/Applications/Xcode.app/Contents/Developer/Platforms/MacOSX.platform/Developer/SDKs/MacOSX27.0.sdk` |
| `xcrun --sdk macosx --show-sdk-version` | 27.0 |
| `rustc --version` | rustc 1.91.0 (f8297e351 2025-10-28) |
| `cargo --version` | cargo 1.91.0 (ea2d97820 2025-10-10) |
| `cargo fmt --version` | rustfmt 1.8.0-stable (f8297e351a 2025-10-28) |
| `cargo clippy --version` | clippy 0.1.91 (f8297e351a 2025-10-28) |
| `node --version` | v24.9.0 |
| `pi --version` | 0.85.1 |
| `command -v sandbox-exec` | `/usr/bin/sandbox-exec` |

Rustup reports `1.91.0-aarch64-apple-darwin`, already selected through `RUSTUP_TOOLCHAIN`, with rustfmt and Clippy installed. Nix is not on PATH; it is not needed for these native probes. Linux packaging and E2E still require the Linux/Nix workflow.

## Reproduce

From the repository root, without Nix:

```sh
cargo fmt --check
cargo test --locked
cargo clippy --all-targets --locked -- -D warnings
cargo build --locked
```

Earlier host validation, before the runtime/resource additions: all four commands passed, including **81 unit tests and four native CLI tests**. Linux-specific tests are compiled only on Linux. The opt-in integration tests in [macos.md](macos.md) exercise the normal coordinator/gateway and actual terminal launcher separately from these default tests. Current patch validation is recorded in [the handoff](macos-handoff.md#review-fixes-before-merge).

Run the separate enforcement probes:

```sh
cargo fmt --manifest-path tests/macos-seatbelt/Cargo.toml --check
cargo test --manifest-path tests/macos-seatbelt/Cargo.toml --locked
cargo clippy --manifest-path tests/macos-seatbelt/Cargo.toml --all-targets --locked -- -D warnings
```

Actual: formatting and Clippy pass; **35 tests pass by default (two unit tests and 33 enforcement/session probes)**. Three opt-in Pi tests pass; the retained nested-profile acceptance gate still fails. Five consecutive rounds of the default suite plus all three Pi tests passed using the built Slopbox executable. JavaScript/TypeScript syntax checks pass with `node --check`.

Without `SLOPBOX_TEST_SLOPBOX`, the independent crate uses its probe executable for workers. To exercise Slopbox's actual hidden worker/recovery dispatch:

```sh
SLOPBOX_TEST_SLOPBOX="$PWD/target/debug/slopbox" \
cargo test --manifest-path tests/macos-seatbelt/Cargo.toml --locked
```

These tests run only on macOS and currently require the user's GUI `launchd` domain. They use private disposable directories under `/var/tmp`, fake credential files, live local listeners, and cleared tool environments. Ordinary probes have five-second deadlines; request, tool, launchctl, and cleanup phases have two-second deadlines. The Pi harness gets a host-selected 30-second execution bound, not a guest-selectable timeout extension. Denial assertions require a permission error: a missing file, refused connection, crash, or timeout does not count.

Run the retained nested-role failure separately:

```sh
cargo test --manifest-path tests/macos-seatbelt/Cargo.toml --locked \
  --test seatbelt nested_role_narrowing -- --exact --ignored --nocapture
```

Expected for nested-role acceptance: the narrowed child can access its workspace/tool state and account endpoint, but not harness state or the model endpoint; an inner grant cannot widen the outer policy.

Actual: the second `sandbox-exec` exits **71** before the tool starts:

```text
sandbox-exec: sandbox_apply: Operation not permitted
```

Cargo exits 101. This also happens with explicit `(allow system-mac-syscall (mac-policy-name "Sandbox"))` in the outer profile. Both profiles work when launched independently by the host. The ignored gate is not a passing security test, and the independent role tests are not evidence of nested enforcement.

## Host-supervised tool launches

The shared native supervisor creates a private Unix socket outside the workspace. Only the harness profile can connect. It accepts one bounded argv request per connection: a four-byte length followed by NUL-separated arguments, limited to 16 KiB and 32 arguments. There are no guest-selectable profile, environment, working-directory, descriptor, credential, or approval fields. Arguments follow `--` in a fixed `/usr/bin/sandbox-exec` invocation with a host-owned tool profile; they never execute directly on the host.

The supervisor launches with Darwin `POSIX_SPAWN_CLOEXEC_DEFAULT`, explicit stdio actions, a private tool home, fixed working directory, and a cleared environment. Tests deliberately remove CLOEXEC from a fake host descriptor and verify that the tool still cannot inherit it. Output is combined and capped at 32 KiB. The Pi probe uses this protocol, but it is not a finished streaming tool API.

Passing evidence:

- A sandboxed harness requests a separately sandboxed tool. Workspace/tool-state operations and the selected account endpoint work; model endpoints, harness state, host-control canaries, and the launch socket remain denied to that tool.
- Harness environment markers are absent in tools. Profile-option injection is rejected; requesting a second `sandbox-exec` cannot acquire harness authority.
- Invalid/oversized frames, partial requests, excessive output, failed startup, timeout, disconnect, supervisor shutdown, and ordinary leader exit are bounded. A subsequent valid request works; failed operations are not replayed.
- Two active supervisors keep their endpoint grants separate. Tools cannot reach their own launch endpoint; the other harness cannot reach it either. Stopping one leaves the other usable.

### Descendant cleanup: passing replacement

The original process-group implementation failed: `posix_spawn` can create a new group or session despite denying direct `setpgid`/`setsid` syscalls. That remains a reason not to use process groups as the ownership boundary.

Each tool request now gets a temporary one-shot `launchd` worker in a distinct resource coalition. Apple documents coalition membership as inherited across fork, exec, and spawn, immutable after creation, with IDs that are never reused. The host verifies the worker's Unix peer audit token, distinct coalition, and initial single-task count before sending any tool request. Profile, paths, job label, and internal socket are host-controlled. `KeepAlive=false` and `LaunchOnlyOnce=true` prevent automatic operation replay; `launchctl submit` is deliberately not used because its documented behavior includes restarting failed jobs.

Cleanup uses coalition membership, not parent IDs or process groups. Signals carry the process's kernel version through `proc_signal_with_audittoken`, so a recycled PID cannot redirect a signal. Cleanup finishes only when the kernel's coalition task count reaches zero (or one for the worker cleaning its own children). An empty PID scan alone is not accepted as proof: fork and exec can race enumeration. The outer supervisor can clean up if its worker dies; the worker cleans up if its control connection disappears.

The original regression is now enabled and passing:

```sh
cargo test --manifest-path tests/macos-seatbelt/Cargo.toml --locked \
  --test seatbelt supervisor_cannot_lose_descendants_to_spawned_process_groups \
  -- --exact --nocapture
```

Passing cases include detached groups, spawn-created sessions, orphaned grandchildren with the intermediate parent already reaped, fork-based launches, failed leader exit, disconnect, timeout, shutdown, worker death, and control-connection loss. Cancellation during repeated exec is exercised ten times per suite. Tests also reject a stale process version, refuse to kill the host coalition, deny guest attempts to select another coalition, and leave a second active job running when the first is stopped.

This works as an ordinary user in `gui/$UID` on the recorded host. `bootstrap user/$UID` returned error 5; no root retry was made. Coalition queries and the task-count prefix use private Darwin interfaces, so this is evidence for this OS build, not a cross-version support claim.

### Hard crashes and endpoint leases

The new tests send actual `SIGKILL` to a coordinator process, not merely disconnect its socket. They also kill its worker or leave it stopped with detached tools still running, then recover from a fresh process. Recovery stops the recorded coalition before unloading its job and releasing the broker port. Another live session remains untouched; a live coordinator's lock prevents recovery from claiming its job.

Ownership is recorded atomically in the private job directory before any executable request is sent. Records contain the boot UUID and the never-reused resource-coalition ID. An exclusive lock distinguishes active owners from stale jobs. Invalid records and records from another boot fail closed without unloading the job or releasing its port. The `pending` state precedes authorization of any tool request.

The lease variant declares an IPv4 TCP socket in the job's `Sockets` dictionary, checks it in with `launch_activate_socket`, and leaves launchd holding the reservation across coordinator/worker death. **`LaunchOnlyOnce=true` does not work for this lease:** killing that worker released its socket on this host. The lease variant instead retains demand activation with `KeepAlive=false`. A replacement worker cannot replay an operation: executable requests are absent from the plist, and the original controller's one-use listener is already closed. A test creates demand after worker death and verifies that the tool ran only once.

Lease tests try both `SO_REUSEADDR` and `SO_REUSEPORT`, and prove the same address becomes claimable after cleanup. Tests avoid server-side `TIME_WAIT`; an address-in-use error alone would otherwise be false ownership evidence. The Pi crash test instead verifies a live listener before recovery and connection refusal afterward, because its HTTP connections can leave `TIME_WAIT` state.

These are process-crash probes, not power-loss, logout/GUI-domain teardown, every startup crash window, or cross-version validation. Malformed/other-boot state is retained for investigation, not automatically repaired.

### Shared native session engine

`session.rs` owns separate `tasks` and `leases` journals under a private, locked session directory. Startup recovers a stale directory before reuse; live owners and invalid records are rejected. Shutdown and recovery empty every task coalition before unloading any endpoint-owning job. Uncertain task ownership retains all endpoint leases. Tests cover stopped workers, coordinator/worker death, malformed ownership, and preservation of another live session.

`relay.rs` forwards the leased IPv4 TCP endpoints to fixed host-selected general/model/account Unix sockets. It bounds connections and buffers, preserves half-closes and backpressure, and never retries requests. Session transport revocation closes existing streams while launchd retains the addresses; this is separate from destination-approval revocation. Tests verify route separation, megabyte transfers, and revocation on both sides of an active stream.

Slopbox now contains hidden execution, relay, and recovery entry points. Workers require launchd parentage and a private controller directory; direct invocation fails before reading host configuration. Executable authorization still follows peer-token/coalition verification and journal persistence. The public launcher now connects this engine to `src/session.rs`, `GatewaySession` and production Pi preparation. The conformance driver still uses fixture profiles and fake brokers; separate integration tests exercise the production path.

## Observed enforcement

| Probe | Expected | Actual |
|---|---|---|
| Fixed `/usr/bin/true` under deny-default | Starts | Pass |
| Workspace and tool-private state | Read/write permitted | Pass |
| Unrelated files, harness configuration, host-control canaries, synthetic SSH/AWS/Keychain/Pi credential roots | Read/write denied; bytes unchanged | Pass |
| File/directory symlinks, `/var` and `/private/var` aliases, case aliases on this filesystem, descendant reads | No wider access | Pass |
| Replace granted workspace with a symlink after child startup | Outside target denied | Pass |
| Known TCP model/general/account/other-session endpoints | Only explicitly granted IPv4 TCP ports reachable; disabling general access retains account access | Pass for independent roles and supervised tools |
| Direct documentation/private IP connections, mapped IPv6 model address, local bind | Permission denied | Pass |
| Unix sockets, including an unrelated socket inside writable workspace and a symlink to the model socket | Only explicitly granted socket reachable | Pass |
| Signal permission check against host supervisor, using `kill(pid, 0)` | Denied without delivering a signal | Pass |
| Child attempts to reapply harness profile | Cannot acquire harness authority | Reapplication rejected, exit 71 |
| Concurrent fixtures, known endpoint addresses, cross-state reads, cleanup of one fixture | Separate grants; surviving fixture remains usable | Pass; not production session/approval lifecycle coverage |

Two additional probes characterize constraints rather than establish isolation:

- An intentionally inherited open descriptor can read an otherwise denied canary. Marking that descriptor close-on-exec makes it unavailable. Seatbelt is not a substitute for closing **all** unintended descriptors before every role launch. The shared supervisor and native harness worker use `POSIX_SPAWN_CLOEXEC_DEFAULT`; the terminal worker receives only three explicit stdio descriptors.
- `(remote ip "localhost:PORT")` grants both `127.0.0.1:PORT` and `[::1]:PORT`, and also permits UDP to that port. The broad-rule characterization is retained, but the supervisor profiles no longer use it.

## Native endpoint narrowing

The tested rule is:

```scheme
(allow network-outbound
    (require-all
        (socket-domain AF_INET)
        (remote tcp "localhost:PORT")))
```

An approved IPv4 TCP listener remains reachable while an unrelated IPv6 listener and UDP socket on the same numeric port are denied. IPv4-mapped IPv6 is also denied. Numeric addresses such as `127.0.0.1:PORT` are rejected by the profile parser (`host must be * or localhost`); denying AF_INET6 through `system-socket` alone did not prevent the connection. The family constraint must be part of the tested outbound rule.

The original fake-endpoint tests retain an in-process reservation after revocation or serving-thread failure. The shared session engine now uses launchd-held leases, including in the Pi tests, and closes revoked relay streams without releasing those leases. The normal gateway now passes the native model/tool integration test, including general-egress denial. That checkpoint did not enable the host approval TUI; the follow-up now uses the shared view. Pawel reports the [native approval and actual-Pi PTY tests](macos.md#host-approval-view) passing.

## Native Pi RPC probe

Use reviewed absolute paths to the installed Node executable and Pi CLI; no package installation or real provider login is involved:

```sh
cargo build --locked
SLOPBOX_TEST_SLOPBOX="$PWD/target/debug/slopbox" \
SLOPBOX_TEST_NODE=/Users/pawel/.local/share/mise/installs/node/24.9.0/bin/node \
SLOPBOX_TEST_PI_CLI=/opt/homebrew/Cellar/pi-coding-agent/0.85.1/libexec/lib/node_modules/@earendil-works/pi-coding-agent/dist/cli.js \
cargo test --manifest-path tests/macos-seatbelt/Cargo.toml --locked \
  --test seatbelt native_pi_ -- --ignored --nocapture

node --check tests/macos-seatbelt/pi-session.mjs
node --check tests/macos-seatbelt/pi-extension.ts
```

Actual: all three tests pass with Node 24.9.0 and Pi 0.85.1. `pi-session.mjs` supplies fake Unix account and OpenAI-compatible SSE brokers and drives the real Pi CLI over RPC through the native session's leased TCP endpoints. `pi-extension.ts` sends bounded tool requests over the harness-only Unix socket; it never falls back to local execution. The harness and tool launches both use verified coalitions and explicit descriptor closure. The harness profile grants execution of the selected Node binary and read access to the installed Pi package and adapter. Tool runtime reads cover `/bin/sh` and the probe executable, with writable workspace/home and the fake account route.

Passing evidence:

- A synthetic model response calls Pi's bash tool. The sandboxed tool writes its workspace, uses the fake account endpoint, and returns its result for a second model response. Exactly two model requests occur.
- Model endpoint access, harness/control/outside canaries, adapter modification, and tool-supervisor access return permission errors in both model-called bash and RPC `user_bash`. Harness environment markers are absent in tools.
- RPC `abort_bash` cancels a command with a detached descendant; the host verifies that descendant was reaped.
- Pi writes history only to its fixture session directory. Project-local extensions/settings cannot run, and adapter files remain outside writable guest state.
- Two Pi sessions complete concurrently with separate configurations, endpoints, and cleanup.
- After a completed model/tool round trip, the coordinator is killed while Pi has a detached tool running. Endpoint listeners remain reserved until a fresh Slopbox process recovers the session; the descendant is reaped.

RPC exercises the same `user_bash` hook used by interactive `!`/`!!`; it does **not** validate literal terminal input, Ctrl+C, rendering, resize, suspension, or host approval prompts. The fixture enables only the overridden bash tool and imports no host Pi resources. Fixture profiles and the test adapter still replace production policy/layout/Pi preparation. These probes are not public-launch acceptance; the newer integration tests described below cover that path.

## Normal launcher integration

`tests/native/cli.mjs` drives the normal CLI dispatch, coordinator, gateway, production adapter and generated profiles. The test binary replaces only OpenRouter's upstream with a local synthetic service; production builds contain no override. The expanded none/data/trusted/read-only matrix covers upstream file tools and trusted fixture imports, followed by model-called bash. RPC bash also succeeds. Tools receive no model markers, cannot read outside/harness canaries or reach the model port (`EPERM`), and receive `SLOPBOX_EGRESS_DENIED` for an unapproved general destination. Cancellation reaps the command's descendant; session files are removed after exit.

`tests/native/terminal.py` launches the built Slopbox executable in a PTY. Literal `!`/`!!`, Escape cancellation, Ctrl+D exit, resize, suspension/resume and terminal restoration after normal exit/SIGTERM/SIGHUP pass. Pi 0.85.1 Ctrl+D exits; single Ctrl+C clears the editor; Escape cancels bash. The terminal bridge uses `select(2)` on Darwin because `poll(2)` rejects terminal devices. Linux retains `poll(2)`.

Native launch uses explicit host runtime paths, per-session homes, a read-only adapter, cleared environments and host-selected tool limits. The worker executable is copied outside the writable workspace before any guest starts. Startup/recovery is serialized while ownership is published. Normal harness execution has no fixture's 30-second bound; tool output remains buffered and capped at 32 KiB. Node model calls require `NO_PROXY` for the local model endpoint, and system curl requires literal reads of `/private/etc/ssl/openssl.cnf` and `cert.pem`.

These tests use no real model credentials, upstream account or host resources. They do not establish Xcode compatibility, full feature parity, or recovery from every startup/power-loss window.

## First real Codex run: transport failure

Pawel's first real-account launch reached interactive Pi and a model-called bash read of the handoff, but requests intermittently failed with `fetch failed`. This is partial progress, not a successful complete real-account acceptance run.

A local reproduction enlarged the model prompt to 128 KiB. The gateway failed before reaching the fake upstream with `Resource temporarily unavailable (os error 35)`: Darwin accepts inherited the listener's nonblocking flag, but the HTTP handlers used blocking reads. Small fixtures had masked the race. Accepted general/model/account connections now explicitly switch to blocking mode; listeners remain nonblocking. A unit test covers fragmented headers, and the larger production-path model/tool test passed three consecutive runs after failing before the fix. No policy grants or credentials changed. Pawel subsequently reported that a fresh real Codex session worked.

The same validation round exposed two intermittent failures while suites ran concurrently: an exit timeout in `native_terminal_round_trip`, and a post-cleanup port-rebind `AddrInUse` in `crash_recovery_does_not_stop_a_second_live_session`. The subsequent targeted fixes and host validation are recorded below.

## Terminal color regression

The native launcher forced `TERM=xterm-256color` and dropped `COLORTERM`, making Pi approximate its dark theme with much brighter tool backgrounds. It now preserves both host terminal capability values, as Linux does. The PTY regression failed before the fix and passes with `COLORTERM=truecolor`, with `TERM=xterm-ghostty` alone, and with a 256-color fallback.

Failure cleanup also closes the PTY before waiting for the killed process: unread output had stalled `waitpid` and hidden the color assertion. This does not resolve the earlier intermittent normal-exit timeout.

## Runtime profile and references

The probe profile is deny-default, grants only explicit workspace/private-state paths plus system runtime reads, and grants no Mach lookup, desktop, Keychain, user-preference, or broad home access. It does not import Apple's broad `system.sb` profile or adopt Anthropic's policy defaults.

On this OS, dyld requires read access to the literal `/` directory and the Cryptex ancestor directories, not just metadata access. The initial profile aborted in `ignition_halt` with SIGABRT (shell exit 134); allowing the documented directory opens fixed the harmless launch. Reading the root directory permits listing its entries, not recursively reading their contents. Required system reads include `/usr/lib`, `/System/Library`, and `/System/Volumes/Preboot/Cryptexes/OS`. Literal metadata permissions for `/var` and its canonical ancestors allow alternate path traversal without opening unrelated trees.

The Pi harness additionally needs literal metadata access to its runtime-path ancestors and reads of `kern.hostname`, `kern.version`, and `hw.machine`. Without those `uname` sysctls, Node 24.9.0 aborts in `node::os::GetOSInformation` while importing `node:os`. The fix is these specific read grants, not unrestricted sysctl or Mach access. Pi RPC needed no Mach lookup grants.

Sources checked:

- `man sandbox-exec`, `man 3 sandbox_init`, `man 7 sandbox`, `man 8 sandboxd`. The tools are deprecated; `sandbox(7)` explicitly warns that already-open descriptors retain authority. The local `sandbox_init(3)` also warns against its named legacy profiles for programs built against SDK 27 or newer; these probes use custom profiles instead.
- `/System/Library/Sandbox/Profiles/dyld-support.sb`, `system.sb`, and `bsd.sb`, particularly the dyld root/ancestor read requirements.
- The selected SDK's `sys/fcntl.h`, `sys/signal.h`, and `sys/spawn.h`, plus `man posix_spawnattr_setflags`. The latter documents `POSIX_SPAWN_CLOEXEC_DEFAULT` closing every descriptor not explicitly included in file actions.
- `man posix_openpt` and the SDK's `sys/ttycom.h` for Darwin PTY acquisition; `/usr/bin/diff --help` for portable stage-diff flags.
- `man launchctl`, `man launchd.plist`, `man launch_activate_socket`, and the selected SDK's `launch.h`, `libproc.h`, `sys/un.h`, and `sys/event.h`. The latter explicitly marks `NOTE_TRACK`/`NOTE_CHILD` unsupported; ordinary kqueue fork notifications are not an automatic descendant tracker.
- Apple XNU revision [`f6217f891ac0bb64f3d375211650a4c1ff8ca1ea`](https://github.com/apple-oss-distributions/xnu/tree/f6217f891ac0bb64f3d375211650a4c1ff8ca1ea): `doc/observability/coalitions.md`, `bsd/kern/kern_exec.c`, `bsd/kern/proc_info.c`, `bsd/kern/sys_coalition.c`, `osfmk/kern/coalition.c`, and the libproc/spawn/coalition headers and wrappers. These establish spawn-attribute behavior, privileged coalition reassignment, version-checked signaling, and size-limited task-count queries. The probes validate those mechanisms on this host.
- Anthropic sandbox-runtime revision [`6fa731368807419ee157f9a3fac955fefe1019c6`](https://github.com/anthropics/sandbox-runtime/blob/6fa731368807419ee157f9a3fac955fefe1019c6/src/sandbox/macos-sandbox-utils.ts), resolved with `git ls-remote` from the handoff's repository URL, which now redirects to `anthropics/sandbox-runtime`. Its broad-read default and additional IPC grants are not Slopbox's contract. Its endpoint rules informed the explicit port/socket probes.
- Pi 0.85.1's installed extension, custom-provider, environment-variable and RPC docs; `examples/extensions/ssh.ts`; and the RPC/interactive `emitUserBash` call sites. Node [v24.9.0 `src/node_os.cc`](https://github.com/nodejs/node/blob/v24.9.0/src/node_os.cc), and Apple's [Libc `gen/uname.c`](https://github.com/apple-oss-distributions/Libc/blob/main/gen/uname.c) explain the additional sysctl reads.

## Compilation boundary

The initial build failed at Linux `FICLONE`/`close_range`, terminal `pipe2`/`TIOCGPTPEER`, and Darwin's ioctl argument type. These source-porting errors are now resolved:

- Linux namespace, mount-table, reflink, runtime, and namespace-probe code is compiled only on Linux. The coordinator selects native platform entry points without duplicating the session pipeline. Native Pi preparation supplies host paths instead of Linux mount remapping.
- `src/backend/macos.rs` requires explicit host runtime configuration and rejects unsupported launch capabilities before resolving secrets. Direct `tool-run` and Linux init entry points remain disabled. Linux policy validation now precedes account-secret resolution as well.
- `status` and `doctor` report native runtime requirements instead of claiming Nix exposure. CLI tests use an invalid synthetic OAuth store and an unresolved account secret to verify that inspection and unsupported launches do not parse credentials, execute projects, or create setup/session state.
- Terminal diagnostic pipes use Rust's close-on-exec pipe API. Darwin PTY acquisition uses `/dev/ptmx` and `TIOCPTYGNAME`; unit tests verify CLOEXEC, dimensions, and Darwin EOF behavior. Interactive restoration passes the earlier actual-CLI PTY test. Pawel also reports the new [approval-view and actual-Pi PTY cases](macos.md#host-approval-view) passing.
- Mac stage copying uses byte copying, and diff selects `/usr/bin/diff` directly with portable `-u`/`-L` flags. Native clipboard capture explicitly errors; it does not fall back to Wayland or expose pasteboard services.
- Two existing Git-route tests now canonicalize their temporary workspace paths, matching the coordinator's contract and handling `/var` versus `/private/var` on this host.

No Linux tests, package checks, or contained-runtime E2E were run on this Mac; only the Darwin Rust target is installed. Run all three Linux CI jobs before merging these shared-code changes.

## Remaining work

Linux CI must pass before merging. Keep the native path explicitly experimental and narrow. Pawel reports the native Git signing/URL-rewrite [disposable host test](macos.md#git-signing-and-account-routes) passing; live repository reads and a real broker-signed checkpoint now pass too. Broader host-resource compatibility, Mach/XPC delegation, broader process controls, remaining startup/power-loss/logout windows, visual approval-view feedback across terminal emulators, unsigned Xcode builds, clipboard and simulator/device access remain separate work. Do not turn those into prerequisites for using or validating the basic Pi/bash path.

### Follow-up investigation: intermittent terminal exit and port rebind

Current sandbox notes:

- Read the native macOS handoff and reproduced the real model/tool/model path far enough to run bash from this Slopbox session. A disposable workspace write/read/remove check succeeded.
- At that stage, Git and Cargo were blocked. The runtime, Rustup and Homebrew fixes below resolved ordinary development-tool access; launchd/PTY validation still requires a host terminal.
- The terminal timeout was reproduced as Pi remaining at an empty prompt after bash cancellation while the test tried to exit with Ctrl+C. Pi 0.85.1 documents Ctrl+D as exit and Ctrl+C as clear/exit depending on editor state, so the test now uses Ctrl+D for the normal-exit path.
- The post-cleanup rebind failure looks consistent with launchd releasing the socket asynchronously after `bootout`/recovery returns. Ownership cleanup still happens before lease release; the tests now wait briefly for the address to become bindable instead of treating the first immediate `EADDRINUSE` after cleanup as definitive.

Host validation after these changes:

- `cargo fmt --check` passed.
- `native_cli_tests::native_terminal_round_trip` passed with reviewed `SLOPBOX_TEST_SLOPBOX`, Node and Pi paths after switching the normal-exit path to Ctrl+D.
- `crash_recovery_does_not_stop_a_second_live_session` passed individually.
- The broader default macOS Seatbelt suite passed: 33 passed, 0 failed, 4 ignored, including `crash_recovery_does_not_stop_a_second_live_session`.

The two recorded intermittent failures are now resolved by targeted fixes and broader default-suite validation. Ignored native Pi integration tests, the expected nested-profile failure, Linux CI, package checks and E2E remain separate validation items.

### Native developer tool exposure direction

After the targeted intermittent fixes, host validation also passed `native_cli_tests::native_cli_model_tool_round_trip` with reviewed Slopbox, Node and Pi paths.

A macOS-only `path_entries`/`read_only_paths` configuration was considered and rejected as too divergent from Linux. The desired developer-profile behavior is the Linux-like workflow: sandboxed tools can use host developer binaries through the shared runtime policy, while credentials and host configuration remain private. macOS should implement that shared runtime plan with Seatbelt file grants and PATH construction, not a separate product surface. Native launchd lifecycle/recovery tests remain host-side even after ordinary build tools work in Pi bash.

The first implementation granted arbitrary PATH directories and whole Homebrew prefixes, and incorrectly treated descendant exclusions as protection against ancestor grants. That approach was withdrawn. The current runtime draft selects Homebrew Cellar, activated mise installations, a real installed Rust toolchain, and Apple compiler/SDK directories; it does not export host RUSTUP_HOME or host Cargo state. Cargo gets a private persistent cache and an explicit target linker. Filesystem-planning tests alone are not production Seatbelt/build enforcement evidence.

### Restore normal Pi rather than replace it

Native preparation now leaves Pi's built-in file tools enabled and uses the shared Linux resource discovery/settings filter. None/data/trusted modes select the same supported resources; native paths replace Linux mount targets and grants remain harness-only/read-only. Host authentication, raw settings and session history remain private. Bash continues to use the supervisor; its transport limits are not imposed on built-in file tools. Ordinary AGENTS.md loading is restored, without enabling project extension/settings trust.

Host validation now passes the expanded public CLI integration matrix: actual file tools, an imported extension/npm dependency, permission-error denials, and none/data/trusted/read-only variants. The actual-CLI PTY test also passes. The main crate passes 95 unit tests and four native CLI tests, including 18 Pi preparation/discovery and 10 runtime-discovery tests; formatting, strict all-target Clippy, JS syntax and diff checks pass. These use disposable fixtures and fake upstreams, not real host plugins or model accounts. See [native setup](macos.md) and [current handoff](macos-handoff.md).

### Rustup shorthand launch regression

The runtime resolver treated `RUSTUP_TOOLCHAIN=1.91.0` as a literal installation directory, incorrectly reporting the installed `1.91.0-aarch64-apple-darwin` toolchain as missing. A regression reproduced that error. Lookup now also tries the host-qualified name using rustup's configured default host triple or the native architecture, without falling back to a different version. Tests cover shorthand/default selection, fully qualified names and rejection of symlinked installations.

A fresh production Pi/RPC session with the original `RUSTUP_TOOLCHAIN=1.91.0` then ran the installed Cargo and rustc 1.91.0 through supervised bash and exited cleanly. This checks discovery and executable access, not a full Cargo build. The launcher was rebuilt without installing software or changing host configuration.

### Homebrew dependency-link regression

The restarted session passed an offline dependency-free Rust compile/run and upstream Pi read/write/edit checks. Git and ripgrep instead aborted on the pcre2 dylib's `opt` install name: the canonical Cellar file was readable, while the opt path returned EPERM. The new `tests/native/homebrew.mjs` smoke reproduced that failure in the running profile.

The patch discovers validated opt symlinks into installed kegs and emits literal link-read grants with ancestor metadata, not recursive opt/prefix grants. Three new discovery/profile tests pass alongside the existing ten runtime and eighteen Pi tests; formatting, strict all-target Clippy and JS syntax pass. New host-only tests use the production renderer for a dylib consumer, permission-error denials, post-application symlink retargeting, and harness separation, then exercise actual Homebrew Git/ripgrep through Pi and the supervisor. Pawel ran both host tests successfully and restarted Pi. The agent then verified Git 2.53.0, ripgrep 15.1.0, `git status`, and the Git-init/status/ripgrep smoke in the live sandbox. The regression is resolved on this host. Reproduction commands remain in [native setup](macos.md#homebrew-opt-link-regression).
