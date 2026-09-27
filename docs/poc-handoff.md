# POC handover

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

[Portable Nix-backed Linux](poc-linux-runtime.md) removes NixOS layout assumptions. The runtime resolver now supplies explicit read-only paths, guest links and PATH to enforcement. Stock Ubuntu with single-user Nix passed the full suite without host-system-link or Nix-configuration changes. Nix-less runtime discovery is still separate work.

## Subsequent work

Bare launch still initializes each project. Generic runtime discovery, arbitrary native command launch, additional model protocols and production TLS rollout remain outside this POC. The one-day session CA has no renewal, mediated uploads require Content-Length, and pinned or proxy-ignoring clients are not supported by this experiment. Do not weaken enforcement to accommodate them.
