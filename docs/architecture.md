# Modular architecture

The [project direction](direction.md) is authoritative. Harness integrations belong outside this repository. This document distinguishes existing implementation from the target boundary; moving Pi into a Rust module was not sufficient extraction. Preserve working enforcement while removing that coupling.

Pi preparation, Linux execution, and the current providers are extracted. `harness/pi.rs` returns a typed harness plan. `session.rs` owns policy, workspace/stage state, brokers, and resource lifetimes. `backend/linux/` owns namespace construction, inner-tool enforcement, and the namespace probe. `backend/nix.rs` shares flake selection, realization and closure validation; each backend owns runtime grants and activation. Its execution plan borrows paths and selected broker endpoints, not live broker objects or credentials.

The Linux runtime plan contains read-only paths, guest system links and PATH. Nix closure resolution and opt-in ELF/script discovery produce this same plan; the launcher does not select a store mode or inspect distro shell paths. ELF metadata identifies dependencies but cannot authorize arbitrary host reads: user-installed dependencies need a host-owned prefix grant. Host-owned `runtime.bundles` explicitly grants read-only application trees for runtime-loaded code and data. The resolver validates their contents and discovers native dependencies; it neither executes package-manager discovery nor embeds installer/harness brands. The same plan covers files and trees. Resources outside those grants remain unresolved.

`provider/` owns OpenRouter credentials, Codex login/refresh/storage, fixed model routes, and upstream transport. The gateway delegates model connections; Pi receives only provider identifiers and emits its own synthetic credential markers. General and authenticated HTTP routes remain separate, using shared framing, address checks, and redaction in `http.rs`.

Native backend entry points are selected at compile time. The experimental macOS launcher connects the same coordinator and gateway to separately launched Seatbelt roles, coalition ownership and leased broker endpoints. Host configuration selects either Node/Pi or generic executables. Generic native discovery validates Mach-O/system-library dependencies and supplies literal read/execute grants, system locale data and a private home, without Pi preparation or a separate tool role. Native Pi preparation uses host paths and a supervisor-backed bash adapter instead of Linux mount remapping and nested tool execution. Pi keeps its built-in file tools in the harness role on both platforms; native preparation reuses the shared settings filter and resource discovery rather than replacing Pi tools or plugin loading. Native project environments use selected Nix closure grants and a supervisor-owned command prefix; activation runs after the tool's Seatbelt boundary, never in Pi or the host coordinator. Nix profile roots live with the native session's recoverable control directory. See [native setup and limits](macos.md) and [enforcement evidence](macos-spike.md). This mutually exclusive native launch arrangement is migration work, not the target design. Additional harness adapters must be external; current fixed provider routes still retain Pi conventions, including its Codex originator header, that must be separated from provider protocol requirements.

## Target responsibilities

| Adapter | Owns | Must not own |
|---|---|---|
| Sandbox backend | Filesystem/network/IPC enforcement, execution roles, process lifecycle | Harness settings, provider authentication, approval policy |
| External harness integration (outside this repository) | Harness configuration, resource discovery, tool hooks, SDK provider registration, UI | Host credentials, enforcement, authority to broaden policy |
| Model provider | Host login/refresh, model discovery, permitted upstream requests, provider-specific transport | Terminal UI, backend selection, harness configuration |

Compose generic backend and provider capabilities; do not create a `MacClaudeBedrock` integration or duplicate provider login for each harness. External integrations consume Slopbox's generic interfaces. They are not implementations selected by a built-in harness registry.

Use built-in Rust modules and small traits at the variable boundaries. Use enums/structs for plans and fixed choices. No dynamic plugins, dependency-injection framework, or universal agent SDK is needed.

Policy, approvals, account routes, signing, workspace/stage management, terminal supervision, and session ownership remain shared services. OS-specific terminal/clipboard mechanisms belong in small platform modules, not duplicate session coordinators.

Target core layout, introduced as implementations are extracted:

```text
session.rs                 # composition and lifecycle ownership
policy.rs                  # authority and profile contracts
backend/
  mod.rs                   # contract and selection
  linux.rs                 # bubblewrap
  macos.rs                 # Seatbelt
  vm/                      # concrete VMM adapters as implemented
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
    → backend/role/provider capability checks
    → session layout and execution plan
    → host brokers + generic runtime resources
    → backend launches session
```

The coordinator owns resources and cleanup. External integrations may request resources and restricted execution, but those requests do not grant authority or permit unmanaged services. Inspection uses description/planning APIs without resolving secrets, evaluating projects, logging in, or creating runtime state.

Pass explicit data between layers:

- **Effective policy:** authority after all ceilings; adapters cannot relax it.
- **Session layout:** workspace, private state, runtime, and configuration paths. Distinguish host paths from process-visible paths.
- **Broker connections:** session-scoped endpoints with allowed execution roles, not real credentials.
- **Command launch request:** executable, arguments, environment and requested resources/roles, checked against host policy without harness-specific interpretation.
- **Execution plan:** workspace/runtime/resource access and process roles the backend must enforce.

The common plan must not be a list of bubblewrap flags. Seatbelt does not provide arbitrary bind-mount remapping. Resolve a layout the backend can enforce. External integrations consume those paths rather than assuming Linux mount locations.

Profiles describe policy, not a particular executable. Today `developer` and `contained` select native isolation; `adversarial` requires a microVM. Choosing a VM does not establish the rest of the stronger-profile contract.

## Composition is not universal compatibility

Before launching or resolving credentials, Slopbox checks that the backend can enforce the requested resource/role policy and that the required runtime and broker capabilities are available. Ordinary command execution does not require recognizing or inspecting a harness.

External integrations separately validate their harness's tool routing and protocol behavior. Conformance tests must demonstrate any stronger separation claim; installing an extension or declaring a capability is not proof of enforcement.

Fail with the missing capability rather than falling back to unsandboxed execution, broader access, or silently dropped protocol features. When tool/model separation is requested, model endpoints belong to the outer role, not project tools. The external integration supplies tool routing; Slopbox enforces the narrower policy. Hooks/wrappers require behavioral tests, not just declarations.

External integrations receive model/protocol information and broker connection details, including synthetic credentials where needed. They never receive real tokens or AWS credentials. General network and authenticated account routes remain separate from model access.

A provider is not a wire protocol: Copilot authentication/account discovery is shared while Messages, Responses, or Chat Completions is selected for the harness/model pair. Bedrock also needs its own upstream authentication and streaming handling. Share transport where genuinely common; add translation only for a demonstrated requirement, not a lowest-common-denominator message format.

## Earlier refactor sequence

1. Pi resource/configuration/launch preparation is extracted into `harness/pi.rs`, preserving generated configuration and tests.
2. Session/workspace orchestration is separated from Linux execution and inner-tool enforcement, preserving session-private ownership and cleanup.
3. OpenRouter/OpenAI Codex provider behavior is extracted, preserving authentication, request restrictions, and streaming semantics.
4. Implement the Seatbelt spike, using actual filesystem/IPC differences to refine the contracts.
5. Add image/VM execution and provider capabilities in tested vertical slices. Extract harness adapters into external projects rather than adding them here.

Keep structural refactors separate from new login flows and policy-default changes. No legacy dispatch path, migration layer, or compatibility aliases are needed.

Use focused adapter conformance tests plus end-to-end tests for supported combinations. Verify denied credential reads, denied tool-to-model access, network mediation, cancellation, cleanup, and concurrent sessions. A trait implementation that compiles is not evidence of a supported security boundary.
