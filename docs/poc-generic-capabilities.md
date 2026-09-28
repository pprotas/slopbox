# Generic capabilities POC

**Status: POC acceptance complete on native macOS and Linux.**

This implements the first slice of the [project direction](direction.md), not universal CLI, harness, or runtime compatibility.

## Acceptance

- Define an identity and account route once; select them through host defaults across two unrelated workspaces.
- Host directory rules can override selection, including disabling signing or accounts. Repository configuration cannot grant either.
- Ordinary HTTPS clients use an explicit proxy and session-local trust, without a forge-specific transport configuration.
- Authentication stays host-side and remains bound to the approved origin, path and methods. Unmatched destinations, authority substitution, and disallowed requests fail closed.
- Existing named account routes, Pi separation, and native/Linux enforcement remain intact.

## Transport decision for this experiment

TLS mediation is explicitly enabled per host-owned account route. It is not enabled for existing routes by default and does not modify the host trust store or live user configuration.

The account proxy accepts CONNECT only for configured mediated origins. It presents preissued certificates from an ephemeral session authority. Certificate private keys stay in host memory; the CA signing key is discarded after setup. Only public trust material is exposed to clients. CONNECT authority, TLS server name, and HTTP Host must agree. Decrypted requests enter the existing account router, with the same method/path checks, authentication replacement, checked DNS resolution, upstream TLS verification, and exact-byte secret redaction. The broker does not follow redirects or forward credentials to another origin.

General egress remains a separate capability. This initial account proxy does not turn unknown destinations into automatically approved tunnels. Client certificate pinning and clients that ignore proxy/trust settings are outside this POC, not reasons to disable verification or expose credentials.

A trusted upstream remains a credential recipient. Redaction does not prevent transformed secret reflection, nor does transport mediation infer repository permissions from GraphQL payloads. Provider-side account restrictions remain necessary.

## Scope

Use local disposable upstreams and credentials for tests. Demonstrate the shared transport with independent clients and include negative tests for trust, destination/path/method boundaries, framing and credential reflection. Separate protocol tests from actual sandbox enforcement evidence.

Generic runtime discovery, arbitrary native command launch, additional model protocols, and production rollout of TLS mediation remain subsequent work. This POC must not be presented as solving those boundaries.

## Validation

- Shared configuration tests select one identity/account definition across two workspaces, exercise disabling and exact-workspace ceilings, and reject repository-controlled grants without resolving secrets.
- Protocol tests use disposable local HTTPS upstreams. They cover upstream trust, authentication replacement, exact reflection redaction, encoded responses, redirects, CONNECT/SNI/Host substitution, method/path restrictions and request framing.
- curl and Node's built-in HTTPS proxy support both work with the same explicit proxy and session CA, without forge-specific configuration or a custom CONNECT implementation. This caught a CA/leaf subject collision that rustls-only tests missed.
- The native macOS fixture launches production sandbox workers across two unrelated workspaces with the same account default. It verifies curl/Node access, trust/destination/path/method denials, read-only public trust, host-config and direct-upstream denial, and absence of account secrets/model settings in tools. Its disposable upstream uses the existing test-only HTTP pin; upstream TLS verification is tested separately above.
- The Linux fixture uses production Slopbox and a disposable, verified local HTTPS upstream. One host definition selects accounts and signs independently verified commits across two workspaces; a directory rule disables both in a third. It checks curl/Node and legacy named routes, credential/key/socket/config hiding, read-only trust, direct-network denial, and trust/destination/path/method/Host denials.
- The full Linux end-to-end suite passes, including existing Pi/model separation, approvals, terminal/clipboard, concurrent sessions, Git routing, staged workspaces and contained runtime closure checks. Linux unit tests, TLS client tests and strict Clippy pass. Validation ran on an aarch64 OrbStack Ubuntu VM with Nix-backed system paths and a Nix daemon; this is not a claim of arbitrary non-Nix Linux support.
- Native macOS shared-account, existing Git signing/routing, and `gh` regression fixtures pass, alongside the unit/CLI suite, TLS tests, formatting and strict Clippy. All credentials and signing keys used by these fixtures are disposable.

```bash
cargo test --all-targets
cargo test gateway::tls -- --include-ignored
cargo clippy --all-targets -- -D warnings
cargo fmt --all -- --check

# Supported Nix-backed Linux host; includes the shared identity/account fixture:
nix run .#e2e

# macOS host enforcement, with reviewed local executables:
cargo build
SLOPBOX_TEST_NODE="$(command -v node)" \
SLOPBOX_TEST_SLOPBOX="$PWD/target/debug/slopbox" \
  cargo test native_cli_tests::native_cli_shared_accounts -- --ignored --exact
```

The ordinary suite skips host-dependent fixtures. The TLS integration test requires curl and Node 24.5+ on PATH. See [configuration](configuration.md#shared-host-defaults-and-directory-rules) for the reusable host configuration and client invocation.
