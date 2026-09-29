# Slopbox

Slopbox runs selected development commands in a project sandbox. It keeps host credentials and signing keys outside the sandbox, mediates configured accounts, and denies unapproved network destinations. [Security guarantees and limits](SECURITY-MODEL.md) matter before using it with untrusted code.

The Linux backend uses bubblewrap and requires working user namespaces. It supports a Nix-backed runtime or explicitly selected ELF/script executables and read-only application bundles. The experimental native Apple-silicon macOS backend uses Seatbelt and explicitly selected executables, Mach-O dependencies and bundles. Both backends share the host kernel. Unsupported policy combinations fail closed; no unsandboxed fallback exists.

Slopbox does not embed or configure a coding harness. Pi, Claude Code, and other applications can run as ordinary selected commands if their resources are explicitly granted. Their subprocesses inherit the application's attached account and model authority. **There is no automatic harness/tool separation.** On Linux, a cooperative external integration can explicitly call `slopbox tool-run` for an inner sandbox; generic commands do not do this automatically. macOS has no generic inner tool role. See [project direction](docs/direction.md).

## Install and run

Build with Rust using `cargo build --locked --release`, or on a supported Nix system use `nix build`. On Apple Silicon, the published 0.2.0 Homebrew formula is available from `pprotas/tap` (it predates the removal of the built-in Pi launcher). macOS also builds with Cargo; see [platform notes](docs/platforms.md).

Define host-owned defaults in `~/.config/slopbox/config.toml`:

```toml
default_command = ["bash"]

[policy]
harness = "none"
credentials = "none"

[runtime]
executables = ["bash", "cat", "git"]
```

Use `slopbox` for the default command, `slopbox -- ARGS` to append arguments, or `slopbox run --dev-env none -- COMMAND` for an explicit selected command. Without a default command, bare `slopbox` asks you to configure one or use `run`; it never starts Pi implicitly. Runtime selections, account routes, identity and signing permissions come only from host configuration. A repository's `.slopbox.toml` can narrow policy, not grant host resources. Existing saved project-policy ceilings remain enforced after upgrading from the built-in Pi launcher; there is no automatic reset or migration that expands access.

```bash
slopbox status --verbose      # inspect effective access without resolving secrets
slopbox doctor                # check launch prerequisites
slopbox network events        # inspect denied destinations on the host
slopbox network approve ID    # approve a live-session request
slopbox stage list            # inspect retained staged workspaces
```

See [configuration](docs/configuration.md) for selected runtimes, persistent private application state, HTTPS account mediation, Git URL rewriting and host-side SSH commit signing. `network = "none"` disables general egress but **not** separately attached account routes; `credentials = "none"` disables fixed model routes but not accounts or signing. A staged or read-only workspace does not restrict attached account authority.

## Development

```bash
cargo fmt --all -- --check
cargo test --locked --all-targets
cargo clippy --locked --all-targets -- -D warnings
nixfmt --check flake.nix tests/nixos.nix
nix build
```

On Nix-backed Linux, `nix run .#e2e` tests native containment with synthetic credentials and no model charges. `tests/linux-nixless.py` and `tests/linux-bundles.py` exercise a host without Nix. Native macOS integration fixtures are opt-in and require explicit paths to a built Slopbox binary and reviewed tools. CI runs on `main` pushes, release tags, pull requests targeting `main`, and manual dispatch.

The software is experimental and licensed under [MIT](LICENSE). Historical POC documents record earlier behavior, including a now-removed embedded Pi integration; they are not current feature claims. The [project direction](docs/direction.md) and [security model](SECURITY-MODEL.md) take precedence.
