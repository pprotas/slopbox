# Explicit Linux application bundles

Follow-up to [selected executables without Nix](poc-nixless-linux.md). Adds host-selected application trees for runtime-loaded code, package metadata, plugins and data. It does not infer arbitrary environments or integrate another harness into Slopbox's core.

## Contract

```toml
[policy]
network = "none"
credentials = "none"
harness = "none"

[runtime]
executables = ["~/.local/tools/example/bin/example"]
bundles = ["~/.local/tools/example", "~/.local/tools/python"]
```

Run `slopbox run --dev-env none -- example`. Preferences apply across repositories without project setup. `status --verbose` labels each bundle as a whole-tree read-only code/data grant, without inspecting programs or resolving secrets. `doctor` validates the selected runtime and probes namespaces without executing application code.

- `executables` selects entry points and command aliases.
- `dependency_roots` authorizes discovered dependency files; it does not mount directories.
- `bundles` explicitly grants entire dedicated installation trees at their existing paths. It is not a claim that every file is necessary or trustworthy. Keep credentials and unrelated data out.

All three are host-owned grants, unavailable to repository configuration. Existing `runtime=host`, `harness=none`, non-root-user and Linux-only restrictions remain. No project/contained-runtime fallback is added. Host helpers retain their separate trust requirements.

Bundle traversal rejects workspace, known credential/control and private-guest overlaps, broad system roots, special files, submounts, mutable hard links, broken/escaping links and excessive trees. Internal aliases and links between selected bundles work. Outward native ELF links use existing dependency authorization; other external files must already be selected or belong to another explicit bundle. Symlinks whose `..` traversal changes meaning across an alias are rejected.

Native ELF objects receive dependency discovery without executing them. Already-bundled filenames and SONAMEs can satisfy discovery when a plugin relies on loader paths supplied by its eventual caller. Slopbox does not invent those paths, set `LD_LIBRARY_PATH` or guarantee that arbitrary dynamic loading succeeds. Enforcement still consumes the common read-only paths/links/PATH plan; no new launcher or package-manager boundary was introduced.

The generated root, directory mounts and aliases are read-only in both execution roles. Writable home, temporary files and workspace policy remain unchanged. Package caches are not automatically imported. Bundle validation is startup-only, not a snapshot: host installations must remain stable, including not acquiring service sockets during the session.

## Compatibility limits

Select a small number of complete application/interpreter installations, not individual package files. Distributions may split an interpreter's standard library and configuration across roots; each data tree needs explicit selection. A dedicated installation can avoid that split. Hard-link-based installers need copies rather than mutable links to a cache.

Only native supported ELF semantics are accepted. A bundle containing incompatible ELF test artifacts or unresolved native dependencies can fail preparation even if the application would not load those files. Additional script interpreters, subprocess commands, activation variables, locale/database files and external plugin trees are not inferred. Programs must direct mutable state to private home/tmp or the permitted workspace, not their installation.

Environment-sensitive interpreters should be invoked through their environment's absolute path or installed script entry point. Generated command symlinks do not reproduce every interpreter's prefix/virtual-environment selection behavior.

## Non-Pi harness boundary

`tests/linux-harness.py` runs **unmodified Aider 0.86.2**, with its Python environment and package data selected as bundles. A local HTTPS server supplies deterministic Chat Completions responses through a configured account route. Aider reads and edits files and runs its own test command in two unrelated workspaces. The server verifies host-injected authentication; the guest receives only a synthetic key. No real model service or account is used, and host trust is unchanged.

The test command confirms hidden credentials/control files, read-only installation trees and direct-network denial. Aider's subprocess retains access to the fixed model broker; an explicit `slopbox tool-run` denies it. Separately, the inner probe successfully calls the account-scoped inference route, which intentionally remains available to both roles. The fixture therefore does **not** establish inference isolation for Aider. This is neither automatic harness/tool separation nor a claim that all Aider features are supported.

## Validation

On Linux without `/nix`, using a normal Slopbox build:

```bash
/usr/bin/python3 tests/linux-nixless.py target/debug/slopbox
/usr/bin/python3 tests/linux-bundles.py target/debug/slopbox

# Prepare only a dedicated disposable fixture installation.
/usr/bin/python3 -m venv "$HOME/slopbox-harness-fixture"
"$HOME/slopbox-harness-fixture/bin/pip" install aider-chat==0.86.2
TIKTOKEN_CACHE_DIR="$HOME/slopbox-harness-fixture/tiktoken" \
  "$HOME/slopbox-harness-fixture/bin/python" -c 'import tiktoken; tiktoken.encoding_for_model("gpt-4o")'
/usr/bin/python3 tests/linux-harness.py target/debug/slopbox "$HOME/slopbox-harness-fixture"
```

Aider's pinned release requires Python 3.10–3.12. Installation and tokenizer acquisition are trusted fixture preparation with network access, not a new sandboxed package-acquisition feature.

The bundle fixture uses a custom package and native plugins, including a `dlopen` dependency found through its caller's RPATH. It checks metadata entry points, data files, subprocess resources, aliases, hidden unrelated dependencies, outer/inner read-only mounts, direct-network denial, repository-grant rejection and unsafe links/hard links/sockets/FIFOs.

Local acceptance passes on fresh aarch64 Ubuntu 25.04 without Nix, using distro Python 3.13 for the generic bundle fixture and uv-installed standalone CPython 3.12.12 plus a copy-installed Aider environment for the harness fixture. The original Nix-free fixture and a separate fresh Ubuntu-with-Nix full enforcement run also pass. Linux: 124 unit tests; macOS: 139 unit and 11 CLI tests, native shared-account/signing fixtures, and serial terminal/approval smoke tests. Strict Clippy, Rust/Python formatting, Ruff and workflow lint pass. Parallel native terminal tests timed out; serial retries passed without code changes.

CI is configured to repeat the new fixtures with distro Python on x86_64 Ubuntu 22.04; those remote jobs have not run.
