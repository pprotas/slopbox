# Portable Nix-backed Linux POC

**Status: implemented and validated on stock-layout Ubuntu with single-user Nix.**

This follows the [generic capabilities POC](poc-generic-capabilities.md) and [project direction](direction.md). It removes NixOS layout assumptions; it does not implement a runtime without Nix.

## Acceptance

- Run on a stock non-NixOS Linux host with Nix-installed tools. Do not replace `/bin/sh`, `/usr/bin/env`, create `/run/current-system`, or modify the host's Nix configuration for Slopbox.
- Discover trusted Nix tools through host PATH. Project flakes and closure queries work with standard single-user or daemon-backed Nix installations.
- Construct guest shell/env links from the selected runtime, not the host distribution's binaries. Keep runtime paths read-only and credentials/host services outside both sandbox roles.
- Demonstrate an ordinary non-Pi command, project-runtime execution, staging/diff, signing/accounts and the existing Pi separation suite.
- Preserve macOS behavior and NixOS compatibility. Inspection does not evaluate flakes or resolve secrets.

The Linux runtime plan describes read-only paths, guest system links and PATH; enforcement consumes that plan without selecting a store mode or inspecting host shell paths. Nix supplies dependency closures. The [Nix-free ELF/script follow-up](poc-nixless-linux.md) uses that same plan. It is opt-in, not a fallback that exposes host `/usr`, libraries or home directories.

## Validation

A fresh aarch64 Ubuntu 25.04 OrbStack machine with standard single-user Nix passed the full end-to-end suite. `/bin/sh` remained `/usr/bin/dash`, `/usr/bin/env` remained the distro binary, and neither `/run/current-system` nor `/etc/nix/nix.conf` nor a Nix daemon socket existed. The contained flake/closure test now runs with single-user Nix as well as daemon-backed Nix.

The suite covers generic commands without Pi on PATH, guest shell/env aliases, staging/diff, contained closure restrictions, shared identities/accounts, TLS clients, Pi/model separation, approvals and terminal lifecycle. Linux and macOS unit tests, strict Clippy, Rust/Nix formatting and the native macOS account/Git fixtures pass. CI now has a separate Ubuntu-host enforcement job alongside the existing NixOS VM job; the new remote job has not yet run.

```bash
nix --extra-experimental-features 'nix-command flakes' run .#e2e
```

The host kernel and security policy must permit outer and nested user namespaces. Slopbox does not change sysctls or bypass AppArmor/container restrictions; `slopbox doctor` reports namespace failures. Nix-installed tools still need to be available on host PATH. The package supplies Bash, env/coreutils, diff and bubblewrap; Pi and workload-specific tools remain separate selections.

Generic command execution is not automatic harness/tool separation. Non-Pi harness integration, model protocol compatibility and dynamic runtime/resource discovery remain subsequent work.
