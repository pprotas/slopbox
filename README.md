# Slopbox

Slopbox runs coding agents with explicit access to project files, tools, networks, and external accounts. It keeps host credentials outside the agent and records network access that needs human approval.

The current alpha supports Nix-backed Linux with Pi, using native `developer` and `contained` profiles, tested on NixOS and [Ubuntu with Nix](docs/poc-linux-runtime.md). An opt-in [Nix-free Linux runtime](docs/poc-nixless-linux.md) runs selected ELF executables and scripts, with explicit [application bundles](docs/poc-runtime-bundles.md) for plugins and package data. Unmodified Aider is acceptance-tested; automatic harness/tool separation is not provided. An [experimental native macOS launcher](docs/macos.md) supports explicitly selected Pi/Node and sandboxed bash, not the full Linux feature set. Each backend must independently satisfy its declared security contract.

## What it does

A Slopbox session can provide:

- a short first-run setup and saved host-owned project policy;
- live, read-only, or staged access to one project;
- a private home and package caches;
- concurrent sessions with per-run generated configuration and shared project history;
- a Nix runtime or explicitly selected Linux executable dependencies;
- deny-by-default HTTP and HTTPS networking;
- host-approved session and project destinations;
- fixed authenticated routes whose real credentials never enter the sandbox;
- separate model and tool network capabilities;
- sandbox-only Git remote URL rewriting, explicit identity, and host-brokered SSH commit signing;
- trusted, data-only, or absent Pi resources.

The Linux backend uses bubblewrap and shares the host kernel. It is intended for mistakes, prompt injection, and ordinary malicious userspace—not kernel exploits. See [SECURITY-MODEL.md](SECURITY-MODEL.md) for precise guarantees and limitations.

## Try it

```bash
nix develop
cargo test
nix develop -c cargo run
```

The default command starts Pi in the current project. First run asks how changes should work and confirms access. Use `slopbox init` to reconfigure, or `slopbox -- --continue` to resume Pi.

Inspect the effective policy without starting an agent:

```bash
nix run . -- status
nix run . -- status --profile contained --verbose
```

Check launch prerequisites without starting Pi or evaluating project code:

```bash
nix run . -- doctor
```

On Wayland, **Ctrl+V** in Pi imports a clipboard image through the host and pastes its sandbox-local path without submitting. No desktop sockets are exposed. Use the terminal's normal paste shortcut for text. See [clipboard limits](docs/configuration.md#clipboard-images-on-wayland).

Try the experimental in-session approval view with `slopbox --approval-view` (or `nix run . -- --approval-view`). Press **Ctrl-]** to inspect denials, approve a destination, or revoke a rule. Approval input stays on the host; the view never retries an operation. See [usage and limitations](docs/configuration.md#host-approval-view-prototype).

The separate host CLI remains available:

```bash
nix run . -- network events
nix run . -- network approve <request-id>             # this session
nix run . -- network approve <request-id> --project   # persistent
nix run . -- network approvals
nix run . -- network revoke <rule-id>
```

Staged workspaces are managed from the host:

```bash
nix run . -- stage list
nix run . -- stage diff <stage-id>
nix run . -- stage apply <stage-id>
nix run . -- stage discard <stage-id>
```

[Shared host defaults and directory rules](docs/configuration.md#shared-host-defaults-and-directory-rules) select reusable identities and accounts without repository configuration. Opt-in account TLS mediation supports ordinary HTTPS clients using an explicit proxy and session CA; see [POC scope](docs/poc-generic-capabilities.md).

Add `git_urls` to an authenticated route to use ordinary Git commands through the broker without changing `.git/config`; see [configuration](docs/configuration.md#git-smart-http).

The basic `cd project && slopbox` workflow and revocable network rules are implemented for Pi. The host approval view remains opt-in; native approval and actual-Pi terminal fixtures have passed on Apple Silicon/macOS 27. Broader runtime/resource discovery, macOS tooling and additional harness/provider integrations remain planned.

## Documentation

- **[Project direction — authoritative](docs/direction.md):** general-purpose isolation, integration boundaries, global configuration, and development priorities. Supersedes conflicting older plans.
- [Concepts](docs/concepts.md): identities, accounts, harnesses, projects, sessions, profiles, and grants.
- [User experience](docs/experience.md): the intended launch, approval, recovery, and review workflow.
- [Configuration](docs/configuration.md): current host and project configuration.
- [Architecture](docs/architecture.md): existing backend, harness, and provider boundaries and earlier refactor plans.
- [Platforms](docs/platforms.md): Linux, native macOS/Seatbelt, and VM backend plans.
- [Mac handoff](docs/macos-handoff.md): checkpoint history, validation and next steps for a new Pi session.
- [Integrations](docs/integrations.md): Claude Code, Codex CLI, OpenCode, Bedrock/SSO, and Copilot targets.
- [Roadmap](docs/roadmap.md): focused implementation priorities.
- [Security model](SECURITY-MODEL.md): current enforcement, trust boundaries, and known gaps.

## Development

```bash
nix develop
cargo fmt --check
nixfmt --check flake.nix tests/nixos.nix tests/native/nix/flake.nix
cargo test
cargo clippy --all-targets -- -D warnings
nix build
nix flake check
nix run .#e2e
```

`nix run .#e2e` runs directly on Nix-backed Linux and requires working outer/nested user namespaces. The app supplies its test tools, including Pi. It validates direct and contained runtime paths, with either single-user Nix or a daemon, without making a model request. CI has separate Ubuntu-host and NixOS-VM jobs; the latter requires KVM. The Nix-free job instead builds with distro tools and runs `tests/linux-nixless.py`, `tests/linux-bundles.py` and the pinned Aider fixture `tests/linux-harness.py` on a host without `/nix`. See [bundle test preparation](docs/poc-runtime-bundles.md#validation).

With direnv/nix-direnv configured on the host, review `.envrc` and run `direnv allow` to activate the development shell automatically.

The flake exports packages and development shells for `x86_64-linux`, `aarch64-linux` and Apple Silicon `aarch64-darwin`. macOS can also build directly with Cargo; see [native setup, Nix validation and limits](docs/macos.md) and [enforcement results](docs/macos-spike.md). Native project dev shells activate only in sandboxed tools; entering the host Nix shell does not import its environment into Slopbox. See the native guide for validation results and trust boundaries.

Slopbox is experimental. Profiles are contracts: unsupported combinations fail rather than silently weakening isolation.
