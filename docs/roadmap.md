# Roadmap

The [project direction](direction.md) is authoritative. Harness-specific integrations belong outside this repository, not in optional built-in adapters. Preserve existing security and reliability work. The numbered platform/provider sections below are earlier backlog context, not the execution order or permission to add harness integrations here.

## Immediate architecture work

1. Decouple runtime resources and restricted execution from Pi preparation, preserving enforcement and explicit capability checks.
2. Extract the embedded Pi extension and harness-specific configuration, imports and UI behavior into an external integration. Keep provider authentication and generic brokering in Slopbox.
3. Replace hardcoded Pi launch and mutually exclusive runtime selection with one harness-neutral configuration and command-launch path. Do not silently downgrade existing tool separation.
4. Validate the generic interfaces with ordinary commands and externally supplied integrations before updating the daily-use configuration and Homebrew release.

See [external integration ownership and migration](integrations.md).

## Repository and CI hosting

GitHub is the private writable primary. [`v0.1.0`](https://github.com/pprotas/slopbox/releases/tag/v0.1.0) is published from a signed initial commit; future work uses normal commits and semantic versioning. Pawel reports the old Forgejo push mirror disabled. Its history remains an archive; existing issue links are historical and the issue tracker has not been migrated.

GitHub Actions checks passed for the release candidate, main, and tag: Linux Rust/package builds, Linux enforcement in a NixOS VM, and Apple Silicon Rust/package checks on the macOS 27 `xcode-27` preview image. Hosted native Seatbelt, launchd recovery and PTY conformance remain separate validation work; an ordinary macOS build does not establish native enforcement.

Host GitHub credentials come from `gh auth token`, not repository-owned secrets. Published version tags must not be moved as part of normal development.

## Current baseline

Implemented:

- native Linux `developer` and `contained` profiles on the NixOS-oriented runtime;
- live, read-only, and staged workspaces;
- host and selected Nix-closure runtimes;
- trusted, data-only, and absent Pi resources;
- separate general, model, and authenticated HTTP capabilities;
- host-only session/project network rules, stable IDs, revocation, and structured events;
- OpenRouter and OpenAI Codex model brokering;
- host command/SOPS/environment secrets and fixed authenticated HTTP routes;
- sandbox-only Git URL rewriting and host-brokered SSH commit signing;
- default Pi launch, host-owned project setup ceilings, `status`, and `doctor`;
- Pi shell-tool isolation and session-private runtime files;
- interactive PTY resize propagation and host-mediated Wayland image paste;
- an opt-in host network approval view with fresh mutation confirmations;
- formatting, tests, Clippy, Nix packaging, and E2E checks.

Resize and image paste have been tested interactively with Ghostty on Wayland. The approval view remains opt-in. The `adversarial` profile remains unimplemented and is rejected at launch.

## 1. Modular core and native macOS developer support

Keep OS backends and model-provider protocols around a shared policy/session core. Earlier modularization put Pi preparation in a separate module, but that is not the final boundary: harness-specific behavior must leave this repository. Session/workspace orchestration and broker lifetimes remain shared. Use small generic interfaces, not a plugin framework or a conditional branch for every platform/harness/provider tuple. See [architecture.md](architecture.md).

The first macOS spike is native Seatbelt execution on an M1/macOS 27 host, using Pi as the regression harness. Native macOS/Xcode tooling is a requirement: a Linux VM must not be the only Mac workflow. Use Anthropic's sandbox runtime as a reference, not as a drop-in policy.

Prove filesystem and IPC confinement, broker-only networking, tool/model separation, private session state, and terminal lifecycle. Then validate an unsigned `xcodebuild` with explicit SDK/cache/build-output access. Simulator/device access and code signing are separate capabilities, not reasons to expose the whole Keychain or broadly enable host services.

Port explicit clipboard import while retaining host-owned credentials, approvals, accounts, and signing. No whole-home or AWS-directory sharing, raw SSH-agent forwarding, or silent unsandboxed fallback.

Acceptance: native Pi and project tools work on macOS under the declared developer policy, including a representative Xcode build, without weakening the NixOS/Pi regression baseline. Unsupported capabilities are explained in `status`/`doctor`.

## 2. Portable Linux runtime and VM-isolated workflows

Build a pinned Linux guest image for standard Linux without requiring a host Nix installation, and reuse its contents in a Mac Linux VM. Nix may remain a build tool and optional project environment. Choose existing execution/VMM tools through small spikes rather than implementing a new VMM.

Apple `container` is a candidate for the Mac VM path, not the native developer path. Prove broker transport, blocked direct egress and unrelated host-service access, inner-tool separation, ARM64 image execution, terminal lifecycle, and cleanup against a pinned release on the target host. Host-only networking is not itself a broker-only policy.

Start the Mac VM path with staged/read-only workspaces. Safe export/apply and recovery are release gates; add live sharing after filesystem and concurrency checks pass. The named `contained` profile currently uses native isolation; the planned `adversarial` profile selects a microVM. Do not silently change that taxonomy or infer the full adversarial contract from the presence of a VM.

Acceptance: the image-backed workflow runs Pi and isolated tools on standard Linux and the Mac VM path, preserving brokered credentials and recoverable changes. See [platforms.md](platforms.md).

## 3. Claude Code with Bedrock and AWS SSO

First colleague-facing pairing: Claude Code using Bedrock through a host-owned AWS named profile. The existing `aws sso login --profile ...` browser/Okta workflow remains on the host.

- Resolve and refresh temporary role credentials on the host; never mount `~/.aws` or inject real AWS credentials into the harness.
- Keep account/profile, region, and permitted model or inference-profile selection host-owned.
- Forward supported Bedrock operations and sign upstream requests on the host. Do not expose a general AWS signing service.
- Handle an expired SSO session with an actionable host login instruction, without replaying an interrupted generation.
- Validate streaming, tool calls, cancellation, disconnects, and refresh with the intended work account.
- Prove that project shell/build processes cannot reach the model broker.

Bedrock credential resolution and permitted-request signing are provider work in Slopbox. Claude-specific configuration and tool routing belong in an external integration. Streaming, private state and terminal behavior exercise the generic execution contract; credential confinement cannot depend on a harness-specific exception.

## 4. Copilot as a provider across harnesses

Copilot means the GitHub Copilot model service, not the Copilot CLI. One host-side account implementation may serve supported wire protocols. Any Pi, Codex CLI or OpenCode adapter belongs outside this repository. Claude Code has no native Copilot integration; any gateway-based compatibility work is separate and outside the initial support scope.

Current Pi upstream supports Anthropic Messages, OpenAI Responses, and Chat Completions for Copilot. OpenCode documents Copilot authentication. These establish useful integration points, not proof that every model works with every harness. Current Codex upstream uses Responses for custom providers.

- Start the Copilot broker with Pi to isolate provider work from a new harness integration.
- Keep device authorization, token exchange/refresh, and account-specific endpoint selection on the host.
- Discover models permitted by the account and organization; filter by harness protocol and required tool capabilities.
- Do not silently enable model policies, change organization settings, or misrepresent billing/request metadata.
- Prefer native protocol forwarding. Add translation only for a demonstrated requirement, with explicit behavioral tests.
- Validate Codex CLI against compatible Responses models before declaring that pairing supported.
- Add OpenCode after its configuration and tool-execution boundary are validated.

Organization approval for the OAuth application/client and provider terms remain deployment requirements. A working personal account is not sufficient evidence for colleague onboarding.

See [integrations.md](integrations.md) for the external integration boundary.

## Private-service access (planned)

Handle private-network reachability in the shared network/protocol layer, not through per-CLI adapters. The motivating case is `fj` reaching a private Forgejo instance: the general proxy rejects its reserved destination, while an explicitly configured authenticated route works.

Add host-owned private-service grants with an exact hostname/port and explicitly permitted destination addresses or ranges. Keep private access denied by default. Validate DNS results against the grant and connect to the checked address to prevent rebinding. Retain loopback, metadata, link-local, and other special-address protections; do not add a blanket “allow LAN” switch. Repository policy may narrow these grants, never create them.

Approvals must clearly identify private-network access and remain scoped and revocable. No automatic retry after approval. All proxy-aware clients should benefit from the same grant; clients that bypass proxies require generic transport support, not individual wrappers.

Reachability is not authentication or repository authorization: a grant permits the destination TCP endpoint, not just one API path or virtual host. Keep credentials host-confined and authenticated HTTP policies reusable across clients. Transparent HTTPS credential injection would require TLS termination/interception and a separate, explicit trust design; it is not part of this network-policy change.

Acceptance: `fj` and `curl` can reach the same approved private service without CLI-specific integration; unapproved destinations, out-of-grant DNS changes, and protected address classes remain blocked. Validate authentication separately rather than treating connectivity as a successful authenticated integration.

## Reliability work alongside expansion

Security defects in supported behavior remain immediate work. Do not lose the existing NixOS/Pi regression suite while adding backends.

- Reproduce intermittent cancellation/OS errors when a concrete trace is available.
- Recover sessions and staged work after cancellation, rendering failure, and crashes.
- Add `changes` with automatic stage selection only when unambiguous.
- Make whole-stage apply recoverable and detect concurrent host changes without silent overwrites.
- Strengthen snapshot consistency and metadata handling.
- Clean stale runtime/import files without deleting recoverable work.
- Test provider streaming, cancellation, refresh, and disconnects with real accounts.

Selective apply and stage expiry follow reliable whole-stage application. No compatibility layers, migrations, old aliases, or general plugin framework are required for this expansion.

## Later work

- Account/repository setup that compiles reviewed intent into routes and Git configuration.
- Larger streamed/chunked Git uploads and signing integration tests.
- Additional approval scopes, expiry, and traffic inspection without automatic grants or replay.
- Landlock, seccomp, resource controls, mediated filesystems, and acquire-then-offline workflows.
- An explicitly validated adversarial profile; a VM alone does not establish that profile's full contract.
- Windows, broader provider coverage, and organization policy distribution.

Keep backend primitives and provider quirks out of the ordinary launch workflow. Add abstractions in response to working implementations, not to the size of the target matrix.
