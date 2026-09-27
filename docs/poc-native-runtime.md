# Selected native macOS commands

Host-owned `[runtime].executables` now launches ordinary commands under Seatbelt without Pi configuration. The existing launchd/coalition supervisor, broker leases, clean environment and cleanup remain in use. This is a bounded native runtime, not Linux feature parity.

```toml
[policy]
network = "none"
credentials = "none"
harness = "none"

[runtime]
executables = ["~/.local/bin/example", "cat", "uname"]
```

```sh
slopbox run -- example --version
```

This selection takes precedence over the legacy `[macos]` Pi runtime. It requires a non-root user, `runtime=host`, `harness=none`, and no project-flake activation. Automatic development-environment selection does not activate a project flake in this mode. Host `default_command` enables bare launch through the same execution path; built-in Pi setup is not provided. Host `[environment]` can configure literal client settings and expand public session paths/endpoints. Repository configuration cannot select executables, environment values or broader grants.

## Runtime boundary

- Bare names resolve only in system executable directories. Other installations require absolute or `~/` paths; arbitrary host PATH entries are not trusted discovery sources.
- Selected files receive literal read and execution grants. Ancestor directories receive metadata access, not directory-tree access. Command symlinks retain their selected spelling and canonical target.
- Discovery parses native 64-bit Mach-O executables, including the host slice of universal binaries, without executing them. Only absolute system-library dependencies are accepted. Simple `/bin/sh` and `/bin/bash` scripts are supported.
- Bash, sh and env are infrastructure. The existing native OS library/locale base remains readable; protected root-owned ICU `.dat` files are additionally selected as literal read-only resources. This supports system `Intl.Collator` initialization without importing `/usr/share` or adding a harness-specific resolver.
- Workspace/private-home/private-temporary code may execute. Unselected external executables cannot. Selected installation files cannot overlap the workspace, credential roots or Slopbox control state; setuid/setgid executables and non-system hard-link aliases are rejected, including root-owned aliases. Protected system commands may retain read-only root-owned hard links.
- The private home persists per workspace under its `native-home` state directory, separate from legacy Pi state. Temporary storage is per-run. Host initialization does not traverse or repair guest-controlled home contents; symlinked/non-private roots are rejected, and guests cannot rename or unlink the root. No host application state is imported.
- Generic named Unix IPC is not enabled; only explicitly attached broker sockets are allowed. Signals are confined to the same sandbox, not arbitrary host processes.

Keep selected installations stable during execution. Validation is not an atomic snapshot. Seatbelt denies file access rather than providing a private filesystem namespace: path existence and some host metadata can remain observable. No host login store, Keychain service, shared temporary directory or unrestricted network access is granted.

`bundles`, `dependency_roots`, non-system dylibs, other script-interpreter discovery, named application IPC, project runtime/activation, staged workspaces and dry-run are not implemented for this mode. Select interpreters explicitly and invoke workspace scripts through them. Missing resources remain unavailable; there is no broad-directory or unsandboxed fallback.

Home-scoped socket path grants were rejected during enforcement testing: a host socket created later in the workspace could be moved into the private home and connected to. The regression retains this attack, and named Unix IPC remains denied rather than weakening host-service isolation.

## Authority and evidence

Generic commands and their subprocesses share the outer role's authority. They can use every attached account route and any enabled model route. Native generic mode does not supply `tool-run` or automatic harness/tool separation. The existing Pi adapter retains its separate tool role.

On Apple silicon/macOS 27, the production launcher passes `tests/native/generic.py`: selected Node execution, read-only installation and workspace enforcement, blocked credential/configuration reads, unselected execution, host-socket denial after symlinks/hard links/renames, direct-network denial, within-role child termination, denied host signalling and exit-status propagation. Persistence checks cover repeated launches, cross-workspace read denial, fresh temporary storage, home-root rename denial and guest state symlinks that must not redirect host initialization. Further regressions cover default-command launch, environment expansion without shell evaluation, ignoring an invalid project flake under a selected runtime, and stdout redirected outside the workspace: descriptor metadata succeeds while reopening the output path for reading or writing stays denied.

The shared [Claude Code fixture](poc-claude-code.md) also passes with the unmodified native release: streaming, Read/Edit/Bash and broker/isolation probes in two unrelated workspaces. The deterministic fixture uses disposable credentials. A separate opt-in [live OpenRouter/Haiku test](poc-claude-code.md#live-openrouter-acceptance) also passes with the production package; an additional terminal test validates typed input, clean exit/terminal restoration and conversation resume across separate Slopbox runs. Subscription authentication remains unvalidated. Claude reports cross-session messaging unavailable because named IPC stays blocked.

```sh
cargo build --locked
export SLOPBOX_TEST_SLOPBOX="$PWD/target/debug/slopbox"
export SLOPBOX_TEST_NODE=/absolute/path/to/reviewed/node
cargo test --locked native_cli_tests::native_cli_generic_commands -- --exact --ignored
```

Native follow-up validation passed: 146 unit tests, 11 CLI tests, strict Clippy, 10 TLS tests, and the Darwin Nix package (141 unit tests, 11 CLI tests). Packaged checks passed for generic execution, Claude, Pi model/tool separation, shared accounts, signing, terminal/approval behavior, and live Claude interaction/resume. The earlier 124 Linux unit tests and selected-runtime/bundle/Claude fixtures were not rerun for this macOS-only change.

The earlier packaged terminal test exposed a PTY teardown hang: closing the master before waiting for the killed child fixed it, and three consecutive reruns passed.

Native acceptance requires the logged-in GUI launchd domain described in [macos.md](macos.md). Remote CI and other macOS versions are separate evidence, not implied by these local results.
