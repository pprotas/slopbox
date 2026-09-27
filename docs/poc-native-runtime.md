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
slopbox run --dev-env none -- example --version
```

This selection takes precedence over the legacy `[macos]` Pi runtime. It requires a non-root user, `runtime=host`, `harness=none`, and no project-flake activation. Bare Pi launch/setup is not provided by this mode. Repository configuration cannot select executables or broaden grants.

## Runtime boundary

- Bare names resolve only in system executable directories. Other installations require absolute or `~/` paths; arbitrary host PATH entries are not trusted discovery sources.
- Selected files receive literal read and execution grants. Ancestor directories receive metadata access, not directory-tree access. Command symlinks retain their selected spelling and canonical target.
- Discovery parses native 64-bit Mach-O executables, including the host slice of universal binaries, without executing them. Only absolute system-library dependencies are accepted. Simple `/bin/sh` and `/bin/bash` scripts are supported.
- Bash, sh and env are infrastructure. The existing native OS library/locale base remains readable; protected root-owned ICU `.dat` files are additionally selected as literal read-only resources. This supports system `Intl.Collator` initialization without importing `/usr/share` or adding a harness-specific resolver.
- Workspace/private-home code may execute. Unselected external executables cannot. Selected installation files cannot overlap the workspace, credential roots or Slopbox control state; setuid/setgid executables and non-system hard-link aliases are rejected, including root-owned aliases. Protected system commands may retain read-only root-owned hard links.
- The session has a short, disposable private home and temporary directory. Generic named Unix IPC is not enabled; only explicitly attached broker sockets are allowed. Signals are confined to the same sandbox, not arbitrary host processes.

Keep selected installations stable during execution. Validation is not an atomic snapshot. Seatbelt denies file access rather than providing a private filesystem namespace: path existence and some host metadata can remain observable. No host login store, Keychain service, shared temporary directory or unrestricted network access is granted.

`bundles`, `dependency_roots`, non-system dylibs, other script-interpreter discovery, named application IPC, project runtime/activation, staged workspaces and dry-run are not implemented for this mode. Select interpreters explicitly and invoke workspace scripts through them. Missing resources remain unavailable; there is no broad-directory or unsandboxed fallback.

Home-scoped socket path grants were rejected during enforcement testing: a host socket created later in the workspace could be moved into the private home and connected to. The regression retains this attack, and named Unix IPC remains denied rather than weakening host-service isolation.

## Authority and evidence

Generic commands and their subprocesses share the outer role's authority. They can use every attached account route and any enabled model route. Native generic mode does not supply `tool-run` or automatic harness/tool separation. The existing Pi adapter retains its separate tool role.

On Apple silicon/macOS 27, the production launcher passes `tests/native/generic.py`: selected Node execution, read-only installation and workspace enforcement, blocked credential/configuration reads, unselected execution, host-socket denial after symlinks/hard links/renames, direct-network denial, within-role child termination, denied host signalling and exit-status propagation.

The shared [Claude Code fixture](poc-claude-code.md) also passes with the unmodified native release: streaming, Read/Edit/Bash and broker/isolation probes in two unrelated workspaces. The deterministic fixture uses disposable credentials. A separate opt-in [live OpenRouter/Haiku test](poc-claude-code.md#live-openrouter-acceptance) also passes with the production package; it does not validate subscription authentication or interactive use.

```sh
cargo build --locked
export SLOPBOX_TEST_SLOPBOX="$PWD/target/debug/slopbox"
export SLOPBOX_TEST_NODE=/absolute/path/to/reviewed/node
cargo test --locked native_cli_tests::native_cli_generic_commands -- --exact --ignored
```

Local regression checks passed: 145 macOS unit tests and 11 CLI tests, 124 Linux unit tests, strict Clippy, TLS mediation, Linux selected-runtime/bundle/Claude fixtures, and the Darwin Nix package. Packaged native checks cover generic execution, Claude, Pi model/tool separation, shared accounts, signing and terminal/approval behavior. The packaged terminal test exposed a PTY teardown hang: closing the master before waiting for the killed child fixed it, and three consecutive reruns passed.

Native acceptance requires the logged-in GUI launchd domain described in [macos.md](macos.md). Remote CI and other macOS versions are separate evidence, not implied by these local results.
