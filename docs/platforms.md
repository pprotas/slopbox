# Platforms and backends

[Project direction](direction.md) governs the intended platform contract; [security model](../SECURITY-MODEL.md) describes implemented enforcement. Backends are not equivalent merely because they accept the same policy axes.

| Platform | Current execution | Runtime selection | Limits |
|---|---|---|---|
| NixOS / Linux with Nix | Native bubblewrap, `developer` and `contained` profiles | Nix store or reviewed project closure | Requires user namespaces; host Nix preparation is trusted; developer can read the whole store. |
| Linux without Nix | Native bubblewrap, host runtime | Explicit selected ELF/scripts, dependency roots and whole read-only bundles | No automatic package data or harness/tool inference; opt-in selection and protected host helpers required. |
| Apple Silicon macOS | Native Seatbelt, generic outer commands | Selected native Mach-O/scripts, dependency roots and whole read-only bundles | Experimental; no generic inner tool role, Nix project activation or native staged workspace. |

Ordinary commands on all platforms inherit outer model/account authority. A cooperative Linux integration can explicitly enter `slopbox tool-run`; macOS cannot currently provide equivalent generic role separation. A selected application may be Pi or another harness, but Slopbox neither installs nor configures it. No supported backend silently falls back to unsandboxed execution.

The `adversarial` profile and a separate-kernel microVM backend are **not implemented**. Native backends share the host kernel and do not contain kernel exploits. VM products, Apple `container`, and Windows support require separate design and enforcement tests; an image or VMM alone would not prove the adversarial contract. Package/runtime discovery without manual resources and reusable host defaults across arbitrary installations remain active design problems, not guaranteed portability.

See [Nix-backed Linux testing](poc-linux-runtime.md), [Nix-free runtime](poc-nixless-linux.md), [bundles](poc-runtime-bundles.md), and [native macOS runtime](poc-native-runtime.md) for tested scopes. Older handoff and spike documents record previous Pi-specific prototypes rather than supported integration paths.
