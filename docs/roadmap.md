# Roadmap

[Project direction](direction.md) is authoritative; [security model](../SECURITY-MODEL.md) lists current capabilities and limitations. The embedded Pi integration and implicit launch were removed after 0.2.0. This is planning, not a support claim.

1. Validate the harness-neutral command path on NixOS, ordinary Linux and Apple-silicon macOS; keep local enforcement, account, signing, saved-ceiling and cleanup regressions meaningful. Fix security defects before adding integrations. External harness adapters must independently prove any restricted role they claim.
2. Finish installation and support documentation for NixOS/flake, source-built Linux without Nix and the experimental native macOS backend. State tested architectures, namespace requirements, runtime selection and unsupported combinations explicitly.
3. Harden reusable host defaults, directory rules and account transport. Keep repository configuration narrowing-only and secrets host-side. Expand TLS client compatibility only with explicit trust/origin tests.
4. Design package acquisition, offline execution and application resource discovery without installer-specific grants or silent authority expansion. Nix evaluation and realization remain trusted host preparation until redesigned.
5. Prototype stronger containment and resource limits, including seccomp/Landlock on Linux and a separate-kernel VM for the unimplemented `adversarial` profile. Do not represent native isolation as equivalent to a VM.

Public-release work requires license, history/Actions-log secret review, current documentation and passing platform enforcement tests. Publishing a repository exposes its earlier commits and logs, not just HEAD; keep the repository private if those cannot be audited.
