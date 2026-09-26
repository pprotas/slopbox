# Harness and provider targets

These are implementation targets, not a list of currently supported combinations. Today Slopbox's validated harness is Pi on its NixOS-oriented Linux runtime, with OpenRouter and OpenAI Codex model brokers.

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

## Minimum adapter contract

Before a harness is called supported:

1. Launch it with session-private, host-generated configuration and explicit imported resources.
2. Keep model access out of project shell/build subprocesses; test attempted access rather than trusting a hook's name.
3. Preserve conversation state without importing the host's entire harness home or credential files.
4. Pass a real streaming request, tool-call loop, cancellation, reconnect/refresh, and clean-exit test.
5. Report the actual provider, model, runtime, and capability limits through host inspection.

Hooks, custom providers, and shell wrappers are candidates, not assumed enforcement boundaries. If a harness cannot enforce the required tool boundary through its integration points, keep it experimental or unsupported until another design is validated. Do not silently fall back to sharing the model broker with all project processes.

Basic support does not require Pi's resource discovery or extension ergonomics. Native harness sandboxing may add protection but does not replace Slopbox's outer boundary or host-owned policy.

## Verification sources

Current upstream sources inspected during planning:

- [Pi Copilot provider](https://github.com/badlogic/pi-mono/blob/main/packages/ai/src/providers/github-copilot.ts): Messages, Chat Completions, and Responses protocols.
- [Pi Copilot authentication](https://github.com/badlogic/pi-mono/blob/main/packages/ai/src/auth/oauth/github-copilot.ts): device authorization, token refresh, endpoint selection, and model catalog handling.
- [OpenCode provider documentation](https://github.com/anomalyco/opencode/blob/dev/packages/web/src/content/docs/providers.mdx): Copilot device login and configurable provider endpoints.
- [Codex provider configuration source](https://github.com/openai/codex/blob/main/codex-rs/model-provider-info/src/lib.rs): custom base URLs, Responses transport, and authentication configuration.

Pin released harness versions for implementation and acceptance tests. Repository branch tips are research evidence, not the supported version contract. Claude Code's current Bedrock and gateway documentation still needs verification before selecting its exact routing and credential configuration.
