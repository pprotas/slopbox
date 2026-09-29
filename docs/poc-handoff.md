# POC handover

> Historical design/validation record. Built-in Pi launch and integration have since been removed; use [current configuration](configuration.md) and the [security model](../SECURITY-MODEL.md) for supported behavior.

The [generic capabilities POC](poc-generic-capabilities.md) meets its bounded acceptance criteria. [direction.md](direction.md) remains authoritative; this is not production TLS rollout or universal runtime/harness support.

## Implementation

- `src/session/access.rs`: shared identity/account defaults, canonical directory overrides, explicit disabling and legacy workspace ceilings. Verbose status identifies each setting's host source without resolving secrets.
- `src/gateway/tls.rs`: opt-in CONNECT mediation, ephemeral session trust and CONNECT/SNI/Host binding, reusing account authentication, path/method checks and redaction. Request framing and malformed authorities fail closed.
- Native/Linux tools receive explicit account proxy settings and read-only public trust. Normal proxy/trust settings and the host trust store are unchanged.
- `tests/account-client.mjs` uses Node 24.5+'s built-in HTTPS proxy support; curl exercises the same transport.
- `tests/native/accounts*.mjs` validates native macOS enforcement. `tests/linux-accounts.mjs` and `tests/linux-account-probe.mjs`, included in `nix run .#e2e`, validate Linux enforcement, verified HTTPS upstreams and shared signing across workspaces.

## Evidence

macOS and Linux unit tests, TLS/client tests, strict Clippy and formatting pass. Native shared accounts, existing Git/signing and gh fixtures pass. The full Linux end-to-end suite passes, including Pi separation and contained closure enforcement. See the POC document for commands and environment details. Only disposable test credentials were used.

## Linux runtime follow-up

[Portable Nix-backed Linux](poc-linux-runtime.md) removes NixOS layout assumptions. The runtime resolver now supplies explicit read-only paths, guest links and PATH to enforcement. Stock Ubuntu with single-user Nix passed the full suite without host-system-link or Nix-configuration changes. The [Nix-free follow-up](poc-nixless-linux.md) adds host-selected ELF/script discovery using that same plan, without ambient `/usr` or home mounts. [Application bundles](poc-runtime-bundles.md) add explicit read-only code/data trees, native plugin discovery and unmodified Aider acceptance across unrelated workspaces. [Native Claude Code](poc-claude-code.md) also passes a headless, streamed Anthropic-compatible bearer-gateway fixture with file reads, edits and Bash probes. Neither fixture establishes inference isolation or live provider/subscription compatibility. No harness or package-manager special cases were added to the resolver.

## Native command follow-up

[Generic native macOS commands](poc-native-runtime.md) now consume host-selected executables without Pi configuration. Mach-O validation, literal read/execute grants, system ICU data and private state feed the existing Seatbelt/launchd boundary. The shared Claude fixture passes streaming and file/shell tools natively, using disposable TLS trust and credentials. This is basic isolation with outer account/model authority, not automatic tool separation or native bundle/dylib parity. A separate opt-in live OpenRouter/Haiku test now passes with the production native package in two disposable workspaces; the real key stays host-side. A separate paid PTY test also validates basic interactive use and cross-run resume after deleting the source file. Generic native homes now persist per workspace, separately from legacy Pi state; temporary storage stays per-run. Named IPC remains blocked, and subscription authentication remains unvalidated.

## Local usability preview

`0.2.0-alpha.1` adds host `default_command`, bounded guest environment configuration, explicit-runtime defaults without automatic flake activation, and a native host Keychain secret source. Generic redirected standard files get metadata-only grants; a Node regression verifies that path-based data access remains denied. No harness adapter was added.

The local Homebrew preview is installed through `pprotas/tap`, whose custom remote points at `../homebrew-tap` on `preview/generic-runtime`. Its checksum-pinned `file://` archive lives in `~/Library/Caches/slopbox-preview/`; no release or push occurred. Host configuration was backed up beside `config.toml` and migrated to a Bash default, selected tools and OpenRouter via Keychain, preserving existing exact-workspace Git accounts and signing identities. The existing test key retains its $1 provider cap; application logins were not imported or modified.

Validation: 149 macOS unit tests, 11 CLI tests, strict Clippy, formatting, 10 TLS tests, Darwin Nix packaging, six packaged native regressions, Homebrew install/audit/test, and installed Bash/Node/Git/Claude command checks passed. The installed Claude 2.1.274 also returned a bounded live Haiku response in a disposable workspace using the new configuration and host Keychain source. Linux was not rerun for this preview; this is not a public release or broader runtime compatibility claim.

### Native startup follow-up

The local `0.2.0-alpha.2` preview increases the launchd worker-connect deadline from two to ten seconds and includes the worker phase and service label in timeout errors. A real launchd worker delayed by three seconds reproduces failure with the old deadline and passes with the new one; a failed-worker test checks bounded timeout reporting and cleanup. Coalition authentication and guest permissions are unchanged.

Both new tests, the existing packaged native regressions, host tests/Clippy, Darwin packaging and Homebrew audit/test pass. Installed Claude onboarding rendered under a real PTY from `nix develop` in a disposable workspace, without submitting a prompt. Startup in the actual repository also passed after its configured signing key became available again. This does not add Pi resources to the selected runtime or establish the cause of every launchd startup failure.

## Subsequent work

Bare Pi launch still initializes each project. Nix-free explicit commands need no project setup, but do not provide integrated Pi launch. Automatic resource selection beyond explicit bundles, automatic non-Pi tool separation, macOS application bundles/non-system dylibs, additional fixed model brokers and production TLS rollout remain outside these slices. The one-day session CA has no renewal, mediated uploads require Content-Length, and pinned or proxy-ignoring clients are not supported by this experiment. Do not weaken enforcement to accommodate them.
