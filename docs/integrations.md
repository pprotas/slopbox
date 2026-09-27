# External harness integrations

The [project direction](direction.md#harness-integrations-live-outside-this-repository) is authoritative: this repository provides isolation and generic capability interfaces, not Pi, Claude, Codex or other harness integrations. The earlier harness/provider target matrix is superseded.

## Ownership

Slopbox owns:

- Process, filesystem, network and role enforcement.
- Host-owned policy, explicit runtime/resource grants and private state.
- Credential transport, provider authentication/wire protocols, signing and approvals.
- Harness-neutral interfaces for requesting restricted execution and inspecting effective capabilities.

External extensions or adapters own:

- Harness-specific launch/configuration and resource discovery.
- Tool hooks and routing into Slopbox's restricted execution interface.
- Harness SDK provider registration, settings, UI behavior and conveniences.

External integration code cannot authorize additional host resources or accounts. Host policy remains authoritative. No Slopbox-side plugin framework or per-harness launcher is required by this design.

## Separation contract

An extension routes calls; Slopbox enforces the resulting process boundary. Integration presence alone does not prove that every tool invocation uses that boundary. Test the claimed tool paths, including cancellation and subprocesses, and disclose bypasses or unsupported paths. Do not claim separation for ordinary subprocesses that inherit outer authority.

Requested stronger separation must fail explicitly if it cannot be provided. Ordinary command isolation remains useful without an integration, but is not an automatic fallback from the stronger contract.

## Current migration work

The embedded `assets/pi-extension.ts`, `src/harness/pi*` preparation, hardcoded `Agent::Pi` launch and related configuration/UI coupling still exist. Their presence is an implementation gap, not the intended ownership boundary. Extract harness-specific behavior while retaining generic enforcement and tests. Native tool execution is still wired to the Pi integration; a harness-neutral native interface is not yet implemented.

[Aider](poc-runtime-bundles.md) and [Claude Code](poc-claude-code.md) fixtures exercise ordinary isolation and account transport. The native Claude tests also cover live OpenRouter authentication, terminal use and resume. They do not establish automatic tool/model separation or arbitrary provider compatibility. Such acceptance tests may remain here without shipping the harness's integration code.

Pin tested harness and external-extension versions. Distinguish provider protocol/authentication support from harness behavior; neither implies the other.
