# Selected Linux executables without Nix

**Status: implemented; bounded acceptance passes on a fresh Ubuntu installation without Nix.**

Follow-up to [portable Nix-backed Linux](poc-linux-runtime.md). This slice supplies a host-selected runtime for ordinary Linux commands, not automatic language environments or harness integration. The subsequent [application-bundle slice](poc-runtime-bundles.md) adds explicit read-only resource trees and non-Pi harness acceptance.

## Use

Build with a normal Linux Rust/C toolchain, outside Nix. Install Slopbox outside the workspace (`cargo install --path . --locked` copies the executable). Distro bubblewrap, Bash, coreutils/env and glibc's `ldconfig` must be available. Staging diffs need diffutils; signing needs OpenSSH. Run as a non-root user on a kernel that permits outer and nested user namespaces.

Configure once in host-owned `~/.config/slopbox/config.toml`:

```toml
[policy]
network = "none"
credentials = "none"
harness = "none"

[runtime]
executables = ["curl", "git", "~/.local/tools/example/bin/tool"]
dependency_roots = ["~/.local/tools/example"]
```

```bash
cd project
slopbox status --verbose
slopbox doctor
slopbox run --dev-env none -- tool
slopbox run --dev-env none -- slopbox tool-run --network none -- curl --version
```

Bare names select protected host executables; absolute or `~/` paths explicitly select user installations. `dependency_roots` authorizes discovered files under an installation prefix, **not a directory mount**. Omit it for ordinary root-owned distro tools. Keep credentials out of approved runtime installations. Neither field belongs in repository configuration. These preferences apply across workspaces without per-repository setup. Existing identity/account defaults and directory restrictions still apply; disabling general/model networking does not disable selected accounts.

## Boundary

Presence of `[runtime]` selects file-level discovery instead of the ambient Nix store. Without it, existing Nix host/project runtimes are unchanged. This mode requires `runtime=host` and `harness=none`; contained/project runtimes never fall back to host tools. Flake activation must be disabled when a flake is present. Bare Pi launch/setup is rejected in this mode instead of silently losing its integration.

The resolver reads native ELF interpreters, NEEDED entries, RPATH/RUNPATH and the protected loader cache without running the selected executable or `ldd`. It supports `$ORIGIN`, absolute script interpreters, and simple `/usr/bin/env name` shebangs with a selected interpreter. ELF metadata describes dependencies, not permission: files must be explicitly selected, beneath a host-authorized dependency prefix, or in a protected root-owned installation.

The existing launcher consumes read-only paths, guest links and PATH. File discovery does not mount `/usr`, library directories, package caches or home directories. Optional `runtime.bundles` separately authorizes dedicated application trees; dependency roots alone never do. Slopbox, bubblewrap, Bash and env are required infrastructure; ssh-keygen is added for signing. Known credential/control roots, workspace overlaps, private guest paths and user-owned hard-link aliases are rejected. The generated root and runtime aliases are read-only; separate workspace/home/tmp mounts retain their intended writability. Inner execution uses the fixed session bubblewrap entry, not a caller-controlled PATH lookup.

Host-side helpers remain a separate authority. They require an existing Nix package or root-owned, non-group/world-writable executables with protected ancestry. Selecting a guest program does not authorize running it as a host credential or discovery helper.

## Limits

- Tested with native 64-bit little-endian glibc ELF on aarch64 Ubuntu/OrbStack; x86_64 Ubuntu CI is configured but has not run remotely.
- Dynamic plugins, application data and language packages require explicit [bundles](poc-runtime-bundles.md). Additional subprocess entry points require selection when not supplied by those bundles. Missing files stay unavailable; this is not a complete Python/Node/compiler environment resolver.
- Relative search paths, `$LIB`/`$PLATFORM`, ELF audit/filter/NODEFLIB semantics, complex env shebangs and oversized/deep graphs fail closed.
- Installations must stay stable during a session. Discovery is not an atomic filesystem snapshot. User-owned hard-linked build outputs need an installed copy outside the workspace.
- Generic commands share their outer role's authority. Only explicit `tool-run` cooperation establishes model/tool separation; arbitrary harnesses do not acquire that boundary automatically.

## Validation

`python3 tests/linux-nixless.py /path/to/slopbox` requires a machine without `/nix`. It covers relocated `$ORIGIN` libraries, RPATH versus RUNPATH inheritance, script interpreters, unauthorized/symlinked dependency rejection, mutable hard links, read-only aliases/files, hidden unrelated tools/data and credentials, direct-network denial, explicit model/tool separation, staged diff, shared HTTPS accounts and verified signed commits across workspaces. Runtime failures precede credential-command execution. Tests use disposable credentials and leave host trust unchanged.

A fresh aarch64 Ubuntu 25.04 OrbStack VM passed this fixture with distro tools and Rust, no `/nix`, no NixOS system paths, and unchanged distro shell/env binaries. All 123 Linux unit tests pass, with and without Nix. The full Nix-backed Linux enforcement suite, 139 macOS unit tests, 11 macOS CLI tests and native account/Git fixtures also pass. Rust formatting, strict Clippy, Python formatting/lint and workflow lint pass (the existing `xcode-27` runner label is allowlisted for actionlint). The new remote CI job has not run.
