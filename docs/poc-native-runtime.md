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
- Discovery parses native 64-bit Mach-O executables, libraries and bundled plugins, including host slices of universal binaries, without executing them. Absolute dependencies, `@loader_path`, known `@executable_path` and inherited `LC_RPATH` stacks are supported. Non-system libraries must belong to explicit bundles or dependency roots. Dependency roots authorize discovered files and their aliases, not directory reads.
- Simple absolute-interpreter and `/usr/bin/env NAME` shebangs require an explicitly selected interpreter. Shell bodies are not dependency manifests; select subprocess entry points explicitly.
- Bash, sh and env are infrastructure. The existing native OS library/locale base remains readable; protected root-owned ICU `.dat` files are additionally selected as literal read-only resources. This supports system `Intl.Collator` initialization without importing `/usr/share` or adding a harness-specific resolver.
- Workspace/private-home/private-temporary code may execute. Unselected external executables cannot. Selected installation files cannot overlap the workspace, credential roots or Slopbox control state; setuid/setgid executables and non-system hard-link aliases are rejected, including root-owned aliases. Protected system commands may retain read-only root-owned hard links.
- The private home persists per workspace under its `native-home` state directory, separate from legacy Pi state. Temporary storage is per-run. Host initialization does not traverse or repair guest-controlled home contents; symlinked/non-private roots are rejected, and guests cannot rename or unlink the root. No host application state is imported.
- Generic named Unix IPC is not enabled; only explicitly attached broker sockets are allowed. Signals are confined to the same sandbox, not arbitrary host processes.

Keep selected installations stable during execution. Validation is not an atomic snapshot. Seatbelt denies file access rather than providing a private filesystem namespace: path existence and some host metadata can remain observable. No host login store, Keychain service, shared temporary directory or unrestricted network access is granted.

Host `bundles` grants entire application trees as read-only code/data, including executable resources. Traversal rejects broad roots, workspace/private-state overlaps, set-id files, hard links, special files, submounts and escaping links. Filesystem identities also reject private paths reached through case aliases. External links need another declared bundle or selected executable; dependency roots alone do not authorize arbitrary linked data.

Keep credentials and unrelated data out of bundles. Discovery is bounded to 100,000 entries, depth 128, 1,024 native graph contexts and 256 MiB per native object. Unresolved dynamic-loader contexts, unsupported shebang arguments and missing resources fail rather than importing extra directories. A standalone plugin requiring an unknown executable's runpaths is not automatically resolvable.

Named application IPC, project runtime/activation, staged workspaces and dry-run remain unavailable in this mode. There is no unsandboxed fallback.

Home-scoped socket path grants were rejected during enforcement testing: a host socket created later in the workspace could be moved into the private home and connected to. The regression retains this attack, and named Unix IPC remains denied rather than weakening host-service isolation.

## Application example

For the tested Homebrew Pi installation, the launcher uses Homebrew Node, not a Node selected through another version manager:

```toml
[runtime]
executables = ["/opt/homebrew/bin/pi", "/opt/homebrew/opt/node/bin/node"]
bundles = ["/opt/homebrew/opt/pi-coding-agent"]
dependency_roots = ["/opt/homebrew"]
```

The dependency root exposes discovered dylibs, not the Homebrew tree. The application bundle exposes Pi's installed code and resources, not its host settings or login. Other applications use the same resource contract; this is not an installer or harness registry.

With a host-defined OpenRouter account opted into `proxy = true`, client configuration can use the ordinary proxy:

```toml
[environment]
OPENROUTER_API_KEY = "slopbox:openrouter"
NODE_USE_ENV_PROXY = "1"
NODE_EXTRA_CA_CERTS = "${SLOPBOX_ACCOUNT_CA}"
OPENSSL_CONF = "/dev/null"
PI_OFFLINE = "1"
```

These are client settings, not Slopbox integration code. `OPENSSL_CONF` avoids importing Homebrew's host OpenSSL configuration; `PI_OFFLINE` suppresses catalogue/update traffic. The tested launcher uses Node 26.8.1. Proxy/trust behavior must be verified for other client runtimes.

```sh
slopbox run -- pi --provider openrouter --model anthropic/claude-haiku-4.5
```

Use a dedicated provider-capped key. The placeholder is not a credential, and selecting Haiku in the CLI is not a broker-enforced model allowlist. Client settings and sessions live in the workspace's private HOME. Host Pi extensions, settings and logins are not imported. A rebuilt Slopbox executable may require fresh macOS Keychain approval.

## Authority and evidence

Generic commands and their subprocesses share the outer role's authority. They can use every attached account route and any enabled model route. Native generic mode does not supply `tool-run` or automatic harness/tool separation. The legacy Pi adapter retains its separate tool role; running unmodified Pi as a generic command does not use it.

On Apple silicon/macOS 27, the production launcher passes `tests/native/generic.py`: selected Node execution, read-only installation and workspace enforcement, blocked credential/configuration reads, unselected execution, host-socket denial after symlinks/hard links/renames, direct-network denial, within-role child termination, denied host signalling and exit-status propagation. Persistence checks cover repeated launches, cross-workspace read denial, fresh temporary storage, home-root rename denial and guest state symlinks that must not redirect host initialization. Further regressions cover default-command launch, environment expansion without shell evaluation, ignoring an invalid project flake under a selected runtime, and stdout redirected outside the workspace: descriptor metadata succeeds while reopening the output path for reading or writing stays denied.

`tests/native/runtime-bundles.py` additionally exercises package data, a native plugin, inherited runpaths, multi-hop dylib aliases, read-only installations, denied sibling dependency data, and private-link/hard-link/set-id rejection. It compiles disposable fixtures with Xcode clang.

The opt-in `tests/native/pi-live.py` runs unmodified Pi 0.85.1 through the ordinary proxy against OpenRouter Haiku. Four streamed turns exercise read/edit/bash and credential, installation and direct-network denial probes. It uses generated data, a disposable HOME, a host-held key capped at $1, 512 output tokens per response, a six-turn cutoff and a 120-second deadline. It does not load the embedded Pi extension. No live test runs in CI.

The shared [Claude Code fixture](poc-claude-code.md) also passes with the unmodified native release: streaming, Read/Edit/Bash and broker/isolation probes in two unrelated workspaces. The deterministic fixture uses disposable credentials. A separate opt-in [live OpenRouter/Haiku test](poc-claude-code.md#live-openrouter-acceptance) also passes with the production package; an additional terminal test validates typed input, clean exit/terminal restoration and conversation resume across separate Slopbox runs. Subscription authentication remains unvalidated. Claude reports cross-session messaging unavailable because named IPC stays blocked.

```sh
cargo build --locked
export SLOPBOX_TEST_SLOPBOX="$PWD/target/debug/slopbox"
export SLOPBOX_TEST_NODE=/absolute/path/to/reviewed/node
cargo test --locked native_cli_tests::native_cli_generic_commands -- --exact --ignored
```

Earlier native follow-up validation passed: 146 unit tests, 11 CLI tests, strict Clippy, 10 TLS tests, and the Darwin Nix package (141 unit tests, 11 CLI tests). Packaged checks passed for generic execution, Claude, Pi model/tool separation, shared accounts, signing, terminal/approval behavior, and live Claude interaction/resume. The earlier 124 Linux unit tests and selected-runtime/bundle/Claude fixtures were not rerun for this macOS-only change.

The earlier packaged terminal test exposed a PTY teardown hang: closing the master before waiting for the killed child fixed it, and three consecutive reruns passed.

The application-resource/shared-proxy slice passes 154 unit tests, 11 CLI tests, 11 TLS tests, strict Clippy and the Darwin Nix package. Seven packaged native regressions cover generic commands, bundles, legacy Pi separation, shared accounts, Git/signing, approvals and terminal cleanup. Packaged Pi live inference and Pi/Claude terminal startup also pass. Linux and remote CI were not rerun for this slice.

Native acceptance requires the logged-in GUI launchd domain described in [macos.md](macos.md). Remote CI and other macOS versions are separate evidence, not implied by these local results.
