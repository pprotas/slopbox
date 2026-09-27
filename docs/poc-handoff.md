# POC handover

Read [direction.md](direction.md) and [poc-generic-capabilities.md](poc-generic-capabilities.md). Work remains uncommitted on `poc/generic-capabilities`; preserve the working tree.

## Implemented

- `src/session/access.rs`: reusable identity/account selections through host defaults and canonical directory rules, explicit disabling, legacy exact-workspace ceilings and secret-free inspection.
- `src/gateway/tls.rs`: opt-in account CONNECT mediation with session-local trust, CONNECT/SNI/Host binding and existing account routing/authentication/redaction checks. CA and leaf subjects are distinct for Node/OpenSSL compatibility.
- Account parsing rejects all Transfer-Encoding, duplicate Content-Length, userinfo and encoded/malformed authorities.
- Linux/native wiring exposes public trust and explicit account proxy settings without replacing normal proxy/trust settings.
- `src/gateway/tls/tests.rs`: registered and passing, including verified local HTTPS upstreams, curl/Node, trust/substitution/framing denials, authentication replacement and redaction.
- `tests/native/accounts*.mjs`: passing native macOS fixture across two unrelated workspaces; production workers enforce read-only trust, host-config/direct-network denial and role environment separation.
- Configuration and security docs describe the experiment, precedence and limitations. No live host configuration or trust stores were changed.

## Validation and remaining work

The macOS unit/CLI suite, explicit TLS integration tests, native shared-account fixture, Pi RPC tests, formatting and strict Clippy pass. Commands are in the POC document.

Linux enforcement has not been run on this host. Run the Linux suite and add equivalent real-client enforcement evidence before calling the POC complete. Native tests use the existing disposable HTTP upstream hook; verified upstream TLS is separately covered by protocol tests. Bare launch still prompts per project. Generic runtime discovery, arbitrary native commands and production TLS rollout remain outside scope.
