# Harness and provider targets

The [project direction](direction.md) is authoritative. These are backlog candidates, not an implementation sequence or a list of supported combinations. Establish the generic contracts before expanding this matrix. Pi and the existing OpenRouter/OpenAI Codex brokers remain regression baselines.

Basic command isolation now has local fixtures for [Aider](poc-runtime-bundles.md) and [native Claude Code](poc-claude-code.md). Claude's fixture uses a deterministic Anthropic-compatible bearer gateway, not Bedrock or a live subscription. These account-route tests do not satisfy the stronger harness/tool-separated contract below.

## Target matrix

| Harness | Initial provider path | Copilot target |
|---|---|---|
| Pi | Preserve existing brokers while validating portable runtimes | First consumer of the new host Copilot broker |
| Claude Code | Bedrock using host AWS SSO/profile authentication | No native Copilot integration; excluded from initial support |
| Codex CLI | Validate its Responses transport and authentication separately from the existing Codex provider | Compatible OpenAI Responses models |
| OpenCode | Validate provider configuration and isolated tool execution | Its documented Copilot integration, with credentials moved to the host broker |

A provider name does not imply that every model, endpoint, or harness feature is interchangeable. Track support per harness version, provider protocol, model capability, and tested account type. Fail clearly rather than silently dropping unsupported thinking, image, tool, or streaming fields.

## Shared account handling, separate wire protocols

The host owns account login, credential storage, refresh, and upstream endpoint validation. A guest receives only the session's narrow model capability. Never delegate a general token-export, AWS-signing, or arbitrary authenticated-request endpoint.

For Copilot, implement account discovery and refresh once. Expose only the model protocols required by the selected harness. Prefer forwarding Anthropic Messages, OpenAI Responses, or Chat Completions without translation when the selected model supports that protocol. Pi upstream already implements all three; that does not establish compatibility with another harness's full request schema. Claude Code has no native Copilot integration. Connecting it through a gateway would be separate compatibility work, not a supported pairing in the initial plan.

Validate account-derived endpoints before sending credentials. Honor the organization's permitted models, OAuth/client approval requirements, rate limits, and billing semantics. Do not automatically change model policies during login. Enterprise account behavior needs its own acceptance test.

For work Bedrock access, the host user runs `aws sso login` with the configured named profile and completes browser/Okta authentication. The broker resolves temporary AWS credentials and signs the permitted Bedrock requests. Neither the SSO cache nor temporary role credentials enter the guest. Region and model/inference-profile selection are host-controlled. An expired SSO session should produce a host-side re-login instruction, not a credential prompt inside the harness.

## Harness/tool-separated integration contract

An integration claiming separate harness and tool authority must:

1. Launch it with session-private, host-generated configuration and explicit imported resources.
2. Keep model access out of project shell/build subprocesses; test attempted access rather than trusting a hook's name.
3. Preserve conversation state without importing the host's entire harness home or credential files.
4. Pass a real streaming request, tool-call loop, cancellation, reconnect/refresh, and clean-exit test.
5. Report the actual provider, model, runtime, and capability limits through host inspection.

Hooks, custom providers, and shell wrappers are candidates, not assumed enforcement boundaries. If a harness cannot enforce the required tool boundary through its integration points, keep it experimental or unsupported until another design is validated. Do not silently fall back to sharing the model broker with all project processes.

Such integration does not require Pi's resource discovery or extension ergonomics. Native harness sandboxing may add protection but does not replace Slopbox's outer boundary or host-owned policy.

## Verification sources

Current upstream sources inspected during planning:

- [Pi Copilot provider](https://github.com/badlogic/pi-mono/blob/main/packages/ai/src/providers/github-copilot.ts): Messages, Chat Completions, and Responses protocols.
- [Pi Copilot authentication](https://github.com/badlogic/pi-mono/blob/main/packages/ai/src/auth/oauth/github-copilot.ts): device authorization, token refresh, endpoint selection, and model catalog handling.
- [OpenCode provider documentation](https://github.com/anomalyco/opencode/blob/dev/packages/web/src/content/docs/providers.mdx): Copilot device login and configurable provider endpoints.
- [Codex provider configuration source](https://github.com/openai/codex/blob/main/codex-rs/model-provider-info/src/lib.rs): custom base URLs, Responses transport, and authentication configuration.

Pin released harness versions for implementation and acceptance tests. Repository branch tips are research evidence, not the supported version contract. Claude Code's gateway configuration/protocol is checked for the pinned fixture above; its Bedrock routing and credential configuration still needs separate verification.
