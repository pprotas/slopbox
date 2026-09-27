# Modular architecture

The [project direction](direction.md) is authoritative. This document records existing boundaries and earlier implementation plans; per-product adapters are not the default expansion strategy. Preserve working enforcement while moving toward the general execution and runtime contracts.

Pi preparation, Linux execution, and the current providers are extracted. `harness/pi.rs` returns a typed harness plan. `session.rs` owns policy, workspace/stage state, brokers, and resource lifetimes. `backend/linux/` owns namespace construction, inner-tool enforcement, and the namespace probe. `backend/nix.rs` shares flake selection, realization and closure validation; each backend owns runtime grants and activation. Its execution plan borrows paths and selected broker endpoints, not live broker objects or credentials.

The Linux runtime plan contains read-only paths, guest system links and PATH. Nix closure resolution and opt-in ELF/script discovery produce this same plan; the launcher does not select a store mode or inspect distro shell paths. ELF metadata identifies dependencies but cannot authorize arbitrary host reads: user-installed dependencies need a host-owned prefix grant. Runtime-loaded resources remain separate, unresolved work.

`provider/` owns OpenRouter credentials, Codex login/refresh/storage, fixed model routes, and upstream transport. The gateway delegates model connections; Pi receives only provider identifiers and emits its own synthetic credential markers. General and authenticated HTTP routes remain separate, using shared framing, address checks, and redaction in `http.rs`.

Native backend entry points are selected at compile time. The experimental macOS launcher connects the same coordinator and gateway to separately launched Seatbelt roles, coalition ownership and leased broker endpoints. Host configuration selects Node/Pi; native Pi preparation uses host paths and a supervisor-backed bash adapter instead of Linux mount remapping and nested tool execution. Pi keeps its built-in file tools in the harness role on both platforms; native preparation reuses the shared settings filter and resource discovery rather than replacing Pi tools or plugin loading. Native project environments use selected Nix closure grants and a supervisor-owned command prefix; activation runs after the tool's Seatbelt boundary, never in Pi or the host coordinator. Nix profile roots live with the native session's recoverable control directory. See [native setup and limits](macos.md) and [enforcement evidence](macos-spike.md). Additional harness selection and integrations remain planned; provider routes still target the existing Pi protocols, including its Codex originator header.

## Responsibilities

| Adapter | Owns | Must not own |
|---|---|---|
| Sandbox backend | Filesystem/network/IPC enforcement, execution roles, process lifecycle | Harness settings, provider authentication, approval policy |
| Harness | Launch/configuration, resource imports, session integration, tool-execution adapter | Host credentials, upstream authentication, authority to broaden policy |
| Model provider | Host login/refresh, model discovery, permitted upstream requests, provider-specific transport | Terminal UI, backend selection, harness configuration |

Compose these modules; do not create a `MacClaudeBedrock` integration or duplicate Copilot login for each harness. A small registry/factory can select implementations. Conditionals should not spread across the coordinator and unrelated adapters.

Use built-in Rust modules and small traits at the variable boundaries. Use enums/structs for plans and fixed choices. No dynamic plugins, dependency-injection framework, or universal agent SDK is needed.

Policy, approvals, account routes, signing, workspace/stage management, terminal supervision, and session ownership remain shared services. OS-specific terminal/clipboard mechanisms belong in small platform modules, not duplicate session coordinators.

Possible layout, introduced as implementations are extracted:

```text
session.rs                 # composition and lifecycle ownership
policy.rs                  # authority and profile contracts
backend/
  mod.rs                   # contract and selection
  linux.rs                 # bubblewrap
  macos.rs                 # Seatbelt
  vm/                      # concrete VMM adapters as implemented
harness/
  mod.rs                   # launch/tool integration contract
  pi.rs
  claude_code.rs
  codex.rs
  opencode.rs
provider/
  mod.rs                   # host model-provider contract
  openrouter.rs
  openai_codex.rs
  copilot.rs
  bedrock.rs
```

Do not create empty implementations for the whole matrix. Existing shared modules move only where the extraction requires it.

## Composition and contracts

```text
host configuration + saved setup + repository restrictions
    → effective policy
    → backend/harness/provider capability checks
    → session layout and execution plan
    → host brokers + generated harness configuration
    → backend launches session
```

The coordinator owns resources and cleanup. Adapters describe requirements and prepare resources; they do not mutate authority or start unmanaged services. Inspection uses description/planning APIs without resolving secrets, evaluating projects, logging in, or creating runtime state.

Pass explicit data between layers:

- **Effective policy:** authority after all ceilings; adapters cannot relax it.
- **Session layout:** workspace, private state, runtime, and configuration paths. Distinguish host paths from process-visible paths.
- **Broker connections:** session-scoped endpoints with allowed execution roles, not real credentials.
- **Harness launch plan:** executable, arguments, sanitized environment, generated resources, and required tool isolation.
- **Execution plan:** workspace/runtime/resource access and process roles the backend must enforce.

The common plan must not be a list of bubblewrap flags. Seatbelt does not provide arbitrary bind-mount remapping. Resolve a layout the backend can enforce, then render harness configuration against it rather than hard-coding Linux paths.

Profiles describe policy, not a particular executable. Today `developer` and `contained` select native isolation; `adversarial` requires a microVM. Choosing a VM does not establish the rest of the stronger-profile contract.

## Composition is not universal compatibility

Before launching or resolving credentials, check that:

- the backend can enforce the requested filesystem/runtime policy and process-role separation;
- the harness routes every relevant project subprocess through the tool boundary;
- the provider/model supports the harness's protocol and required features;
- the actual host supplies the necessary runtime and services.

Fail with the missing capability rather than falling back to unsandboxed execution, broader access, or silently dropped protocol features. Model endpoints belong to the harness role, not project tools. The harness supplies tool routing; the backend enforces the narrower policy. Hooks/wrappers require behavioral tests, not just declarations.

Harness adapters receive model/protocol information and broker connection details, including synthetic credentials where needed. They never receive real tokens or AWS credentials. General network and authenticated account routes remain separate from model access.

A provider is not a wire protocol: Copilot authentication/account discovery is shared while Messages, Responses, or Chat Completions is selected for the harness/model pair. Bedrock also needs its own upstream authentication and streaming handling. Share transport where genuinely common; add translation only for a demonstrated requirement, not a lowest-common-denominator message format.

## Earlier refactor sequence

1. Pi resource/configuration/launch preparation is extracted into `harness/pi.rs`, preserving generated configuration and tests.
2. Session/workspace orchestration is separated from Linux execution and inner-tool enforcement, preserving session-private ownership and cleanup.
3. OpenRouter/OpenAI Codex provider behavior is extracted, preserving authentication, request restrictions, and streaming semantics.
4. Implement the Seatbelt spike, using actual filesystem/IPC differences to refine the contracts.
5. Add image/VM execution and new harness/provider adapters in tested vertical slices.

Keep structural refactors separate from new login flows and policy-default changes. No legacy dispatch path, migration layer, or compatibility aliases are needed.

Use focused adapter conformance tests plus end-to-end tests for supported combinations. Verify denied credential reads, denied tool-to-model access, network mediation, cancellation, cleanup, and concurrent sessions. A trait implementation that compiles is not evidence of a supported security boundary.
