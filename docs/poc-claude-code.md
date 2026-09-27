# Claude Code acceptance

Unmodified native **Claude Code 2.1.283** passes a headless fixture on aarch64 Ubuntu 25.04 without Nix and Apple silicon/macOS 27. Linux needed no core changes. macOS uses the new [generic native executable runtime](poc-native-runtime.md), not a Claude-specific launcher or resolver.

`tests/claude-harness.py` checks:

- Native startup, including command symlinks to version-named executables.
- Anthropic Messages and SSE through a verified local HTTPS upstream. The server waits for Claude's partial output before completing its response, so buffering cannot pass.
- Streamed `Read`, `Edit` and `Bash` calls in two unrelated workspaces without repository configuration. Random read markers, edits and shell-probe output are checked.
- Host-injected bearer authentication, denied host credential/configuration reads, an immutable executable and direct-network denial.
- Account-scoped inference and the fixed-model canary are reachable from Claude's Bash subprocess. On Linux, an explicit `tool-run` denies the fixed-model broker but retains the account route. Native generic mode has no separate tool role.

The upstream is a deterministic fixture, not Anthropic. No real account, subscription, model usage or host trust changes are involved. Reported token costs come from fabricated usage fields, not actual charges.

## Configuration shape

For an Anthropic-compatible gateway accepting bearer authentication:

```toml
[policy]
network = "none"
credentials = "none"
harness = "none"

[runtime]
executables = ["~/.local/bin/claude", "cat", "uname", "sleep", "chmod", "mkdir", "rm", "touch"]

[defaults]
accounts = ["claude"]

[secrets.claude]
source = "environment"
variable = "HOST_CLAUDE_GATEWAY_TOKEN"

[[http_routes]]
name = "claude"
upstream = "https://your-gateway.example"
methods = ["POST"]
authentication = { type = "bearer", secret = "claude" }
```

Claude carries its application resources; no application-tree grant is needed. Select additional development tools explicitly. The Linux isolation probe uses selected Python/standard-library resources; the macOS probe uses selected Node. Neither is a Claude dependency. The fixture enables a disposable fixed-model canary using `credentials=brokered`; normal Claude account use does not require that setting.

Expand the broker URL **inside** the guest, since native macOS allocates its port per session:

```sh
slopbox run --dev-env none -- bash -c '
  export ANTHROPIC_BASE_URL="$SLOPBOX_AUTHENTICATED_HTTP_BASE_URL/claude"
  export CLAUDE_CODE_TMPDIR="$TMPDIR"
  exec "$@"
' fixture env \
  ANTHROPIC_AUTH_TOKEN=slopbox:claude \
  CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1 \
  CLAUDE_CODE_DISABLE_OFFICIAL_MARKETPLACE_AUTOINSTALL=1 \
  claude -p 'Read answer.txt' --model claude-sonnet-4-6
```

`CLAUDE_CODE_TMPDIR` keeps Claude's files in private temporary storage rather than its macOS `/tmp` default. The fixture allowlists `Read`, `Edit`, `Bash` and uses `dontAsk` for unattended operation. These are application settings, not Slopbox enforcement. Network namespaces or Seatbelt enforce denial.

## Native runtime findings

Bun's `Intl.Collator` initialization hit an assertion loop when system ICU data was unreadable. The generic runtime now grants protected system ICU data files literally and read-only, without exposing `/usr/share` or adding Keychain/Mach-service access. Diagnosis used a disposable debug-signed copy; acceptance was repeated with the original checksum-verified binary.

Seatbelt also permits execution independently of ordinary file reads. Generic profiles therefore restrict execution paths explicitly; a regression checks that an unselected system binary cannot execute. Native file tests check denied reads, not Linux-style disappearance: file existence can remain observable on macOS. A home-scoped Unix-socket grant was removed after a socket-rename bypass was reproduced; Claude's headless fixture still passes without named application IPC.

## Limits and reproduction

This validates the **headless bearer-gateway path**, not direct Anthropic `x-api-key` authentication, subscription login/refresh, interactive UI, MCP, plugins, remote control or every Claude feature. Account authentication injects `Authorization`, not arbitrary secret-bearing headers. There is no new fixed Anthropic broker or automatic harness/tool separation. A root-prefix POST route grants the corresponding upstream account authority, not inference-only authority.

Install the pinned release in a disposable directory and verify its SHA-256 before execution. The Linux CI workflow pins the x86_64 artifact; remote CI has not run. Local artifacts from `https://downloads.claude.ai/claude-code-releases/2.1.283/`:

| Artifact | SHA-256 |
|---|---|
| `linux-arm64/claude` | `346d294f0103d6fc0de11ac953579b5c62dfa90698a4cfc486b6f927c615e697` |
| `darwin-arm64/claude` | `d8cb1e5c79684cc12a8bfc813e3a2073406921b6245744b3009be3ab5651d21e` |

Linux:

```sh
/usr/bin/python3 tests/claude-harness.py target/debug/slopbox /absolute/fixture/claude
```

macOS, from a logged-in host terminal:

```sh
cargo build --locked
export SLOPBOX_TEST_SLOPBOX="$PWD/target/debug/slopbox"
export SLOPBOX_TEST_NODE=/absolute/path/to/reviewed/node
export SLOPBOX_TEST_CLAUDE=/absolute/fixture/claude
cargo test --locked native_cli_tests::native_cli_claude_harness -- --exact --ignored
```

The native test driver adds the disposable CA only for an explicitly permitted `127.0.0.1` account upstream. This override is compiled out of production. The worker and Seatbelt enforcement use the production executable; neither trust verification nor host isolation is disabled.

References: Claude's [gateway configuration](https://code.claude.com/docs/en/llm-gateway-connect), [gateway protocol](https://code.claude.com/docs/en/llm-gateway-protocol), and [CLI reference](https://code.claude.com/docs/en/cli-reference).
