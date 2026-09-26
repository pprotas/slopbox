#!/usr/bin/env bash
set -euo pipefail

SLOPBOX=${1:-}
if [[ -z "$SLOPBOX" ]]; then
  echo "usage: tests/e2e.sh /path/to/slopbox" >&2
  exit 2
fi
SLOPBOX=$(realpath "$SLOPBOX")
SOURCE_ROOT=${2:-}
TERMINAL_TESTS_DIR=${3:-"$(dirname "$(realpath "${BASH_SOURCE[0]}")")"}

for command in awk bash bwrap cmp curl diff find git node pi rg script sed stty timeout; do
  if ! command -v "$command" >/dev/null; then
    echo "missing test dependency: $command" >&2
    exit 2
  fi
done

node --test "$TERMINAL_TESTS_DIR/pi-rpc.test.mjs"

root=$(mktemp -d)
cleanup() {
  exec 9>&- 2>/dev/null || true
  for pid in ${active_pid:-} ${second_pid:-} ${follow_pid:-}; do
    kill "$pid" 2>/dev/null || true
    wait "$pid" 2>/dev/null || true
  done
  rm -rf "$root"
}
trap cleanup EXIT

host_home="$root/host-home"
runtime_dir="$root/runtime"
workspace="$root/workspace"
mkdir -p \
  "$host_home/data/slopbox/credentials" \
  "$host_home/config/slopbox" \
  "$host_home/.cache/e2e-resource" \
  "$host_home/.cache/e2e-overlay" \
  "$host_home/.pi/agent/bin" \
  "$host_home/.pi/agent/extensions" \
  "$host_home/.pi/agent/skills/e2e-host-skill" \
  "$host_home/.pi/agent/prompts" \
  "$host_home/.pi/agent/npm/node_modules/e2e-host-package/skills/e2e-package-skill" \
  "$runtime_dir" \
  "$workspace/.pi/extensions"
chmod 700 "$host_home" "$host_home/data/slopbox/credentials" "$host_home/.pi/agent" "$runtime_dir"
printf 'fixture\n' >"$workspace/input.txt"
printf 'mounted resource\n' >"$host_home/.cache/e2e-resource/data.txt"
printf 'overlay lower\n' >"$host_home/.cache/e2e-overlay/data.txt"
cat >"$host_home/config/slopbox/config.toml" <<EOF
profile = "developer"

[[pi.read_only_mounts]]
source = "$host_home/.cache/e2e-resource"
target = "~/.cache/e2e-resource"

[[pi.temporary_overlay_mounts]]
source = "$host_home/.cache/e2e-overlay"
target = "~/.cache/e2e-overlay"
EOF
cp "$host_home/config/slopbox/config.toml" "$host_home/config/slopbox/config.base.toml"
printf 'SLOPBOX_E2E_HOST_AGENTS\n' >"$host_home/.pi/agent/AGENTS.md"
cat >"$host_home/.pi/agent/settings.json" <<'EOF'
{
  "defaultThinkingLevel": "high",
  "quietStartup": true,
  "externalEditor": "must-not-run",
  "httpProxy": "http://must-not-use.invalid",
  "extensions": ["must-not-load.ts"],
  "packages": ["npm:e2e-host-package"],
  "defaultProjectTrust": "always",
  "transport": "websocket"
}
EOF
cat >"$host_home/.pi/agent/extensions/e2e-host.ts" <<'EOF'
import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
export default function (pi: ExtensionAPI) {
  pi.registerCommand("slopbox_e2e_host_extension_loaded", { handler: async () => {} });
}
EOF
cat >"$host_home/.pi/agent/skills/e2e-host-skill/SKILL.md" <<'EOF'
---
name: e2e-host-skill
description: Slopbox host skill fixture.
---
# E2E host skill
EOF
printf 'E2E host prompt\n' >"$host_home/.pi/agent/prompts/e2e-host.md"
cat >"$host_home/.pi/agent/bin/e2e-helper" <<'EOF'
#!/bin/sh
printf SLOPBOX_E2E_HOST_HELPER
EOF
chmod 755 "$host_home/.pi/agent/bin/e2e-helper"
cat >"$host_home/.pi/agent/npm/node_modules/e2e-host-package/package.json" <<'EOF'
{
  "name": "e2e-host-package",
  "pi": {
    "extensions": ["./index.ts"],
    "skills": ["./skills"]
  }
}
EOF
cat >"$host_home/.pi/agent/npm/node_modules/e2e-host-package/index.ts" <<'EOF'
import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
export default function (pi: ExtensionAPI) {
  pi.registerCommand("slopbox_e2e_package_extension_loaded", { handler: async () => {} });
}
EOF
cat >"$host_home/.pi/agent/npm/node_modules/e2e-host-package/skills/e2e-package-skill/SKILL.md" <<'EOF'
---
name: e2e-package-skill
description: Slopbox package skill fixture.
---
# E2E package skill
EOF

codex_canary="slopbox-e2e-codex-canary"
cat >"$host_home/data/slopbox/credentials/openai-codex.json" <<EOF
{"provider":"openai-codex","access":"header.payload.$codex_canary","refresh":"refresh-$codex_canary","expires":4102444800000,"account_id":"account-$codex_canary"}
EOF
chmod 600 "$host_home/data/slopbox/credentials/openai-codex.json"

cat >"$workspace/.pi/extensions/must-not-load.ts" <<'EOF'
import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
export default function (pi: ExtensionAPI) {
  pi.registerCommand("slopbox_e2e_project_extension_loaded", {
    handler: async () => {},
  });
}
EOF

canary="slopbox-e2e-openrouter-canary"
common_env=(
  "SHELL=$(type -P bash)"
  "HOME=$host_home"
  "XDG_DATA_HOME=$host_home/data"
  "XDG_CONFIG_HOME=$host_home/config"
  "XDG_RUNTIME_DIR=$runtime_dir"
  "OPENROUTER_API_KEY=$canary"
)

slopbox() {
  env "${common_env[@]}" "$SLOPBOX" "$@"
}

pi_rpc() {
  local output=$1
  shift
  if env "${common_env[@]}" node "$TERMINAL_TESTS_DIR/pi-rpc.mjs" "$@" >"$output.out" 2>"$output.err"; then
    return 0
  fi
  cat -- "$output.out" "$output.err" >&2
  return 1
}

wait_for_file() {
  local path=$1
  for _ in $(seq 1 100); do
    [[ -e "$path" ]] && return 0
    sleep 0.05
  done
  echo "timed out waiting for $path" >&2
  return 1
}

assert_contains() {
  local pattern=$1
  local path=$2
  if ! rg -q -- "$pattern" "$path"; then
    echo "expected $path to contain: $pattern" >&2
    cat -- "$path" >&2
    return 1
  fi
}

assert_not_contains() {
  local pattern=$1
  shift
  if rg -q -F -- "$pattern" "$@"; then
    echo "unexpected value found: $pattern" >&2
    return 1
  fi
}

echo "e2e: status is read-only and distinguishes authority"
status_home="$root/status-home"
mkdir "$status_home"
env HOME="$status_home" XDG_CONFIG_HOME="$status_home/config" XDG_DATA_HOME="$status_home/data" \
  OPENROUTER_API_KEY= "$SLOPBOX" status --workspace "$workspace" >"$root/unconfigured-status.out"
test ! -e "$status_home/config"
test ! -e "$status_home/data"
assert_contains '^Model +None configured$' "$root/unconfigured-status.out"
slopbox status --workspace "$workspace" --verbose >"$root/status.out"
assert_contains '^Changes +Immediate;' "$root/status.out"
assert_contains '^Host tools +Entire Nix store' "$root/status.out"
assert_contains '^Model +OpenRouter, OpenAI Codex' "$root/status.out"
assert_contains '^Pi resources +Trusted code and data; 2 extension' "$root/status.out"
assert_contains '^Temporary .*e2e-overlay.*writes discarded' "$root/status.out"
assert_contains '^profile: developer$' "$root/status.out"
assert_contains '^trusted-extension: ' "$root/status.out"
assert_not_contains "$canary" "$root/status.out"
assert_not_contains "$codex_canary" "$root/status.out"
slopbox status --workspace "$workspace" --no-host-pi-resources --tool-network none >"$root/restricted-status.out"
assert_contains '^Tool internet General egress disabled; configured account routes remain available$' "$root/restricted-status.out"
assert_contains '^Pi resources +No host resources$' "$root/restricted-status.out"
assert_not_contains 'Host import' "$root/restricted-status.out"
assert_not_contains 'Temporary' "$root/restricted-status.out"
slopbox status --workspace "$workspace" --profile adversarial >"$root/unsupported-status.out"
assert_contains '^Launch +Unsupported:' "$root/unsupported-status.out"

echo "e2e: doctor checks prerequisites without launching or changing project state"
slopbox doctor --workspace "$workspace" >"$root/doctor.out"
assert_contains '^OK +Namespaces: outer and nested tool namespaces work' "$root/doctor.out"
assert_contains '^OK +Model configuration: OpenRouter, OpenAI Codex' "$root/doctor.out"
assert_contains '^0 failed check' "$root/doctor.out"
assert_contains '^Not checked:' "$root/doctor.out"
test ! -e "$host_home/data/slopbox/boxes"
test ! -e "$host_home/config/slopbox/projects"
assert_not_contains "$canary" "$root/doctor.out"
assert_not_contains "$codex_canary" "$root/doctor.out"
mv "$host_home/.cache/e2e-resource" "$host_home/.cache/e2e-resource-away"
if slopbox doctor --workspace "$workspace" >"$root/missing-resource-doctor.out" 2>&1; then
  echo "doctor accepted an unavailable host resource" >&2
  exit 1
fi
assert_contains '^FAIL Pi resources:' "$root/missing-resource-doctor.out"
slopbox doctor --workspace "$workspace" --no-host-pi-resources >"$root/no-resources-doctor.out"
assert_contains '^0 failed check' "$root/no-resources-doctor.out"
mv "$host_home/.cache/e2e-resource-away" "$host_home/.cache/e2e-resource"
cat >>"$host_home/config/slopbox/config.toml" <<EOF

[[http_routes]]
name = "doctor"
workspace = "$workspace"
upstream = "https://doctor.slopbox-e2e.invalid/repo"
methods = ["GET"]
authentication = { type = "bearer", secret = "doctor" }
EOF
if slopbox doctor --workspace "$workspace" >"$root/missing-secret-doctor.out" 2>&1; then
  echo "doctor accepted an undefined secret reference" >&2
  exit 1
fi
assert_contains '^FAIL Account configuration: route doctor refers to unknown secret doctor' "$root/missing-secret-doctor.out"
cat >>"$host_home/config/slopbox/config.toml" <<EOF

[secrets.doctor]
source = "sops"
file = "$root/absent-secret-file"
key = '["token"]'
EOF
slopbox doctor --workspace "$workspace" >"$root/no-decryption-doctor.out"
assert_contains '^OK +Account configuration: 1 route' "$root/no-decryption-doctor.out"
assert_contains '^0 failed check' "$root/no-decryption-doctor.out"
cp "$host_home/config/slopbox/config.base.toml" "$host_home/config/slopbox/config.toml"
if env HOME="$status_home" XDG_CONFIG_HOME="$status_home/config" XDG_DATA_HOME="$status_home/data" \
  OPENROUTER_API_KEY= "$SLOPBOX" doctor --workspace "$workspace" >"$root/missing-model-doctor.out" 2>&1; then
  echo "doctor accepted missing model configuration" >&2
  exit 1
fi
assert_contains '^FAIL Model configuration:.*slopbox auth login openai-codex' "$root/missing-model-doctor.out"
assert_contains '^OK +Namespaces:' "$root/missing-model-doctor.out"
test ! -e "$status_home/config"
test ! -e "$status_home/data"
if slopbox doctor --workspace "$workspace" --profile contained >"$root/missing-flake-doctor.out" 2>&1; then
  echo "doctor accepted runtime=project without a flake" >&2
  exit 1
fi
assert_contains '^FAIL Development environment: runtime=project requires flake.nix' "$root/missing-flake-doctor.out"
if slopbox doctor --workspace "$workspace" --profile adversarial >"$root/unsupported-doctor.out" 2>&1; then
  echo "doctor accepted an unsupported policy" >&2
  exit 1
fi
assert_contains '^FAIL Policy:' "$root/unsupported-doctor.out"
assert_contains '^Namespaces: native probe skipped' "$root/unsupported-doctor.out"
if env "${common_env[@]}" PATH="$(dirname "$(realpath "$(type -P true)")")" \
  "$SLOPBOX" doctor --workspace "$workspace" >"$root/missing-tools-doctor.out" 2>&1; then
  echo "doctor accepted missing Pi and sandbox tools" >&2
  exit 1
fi
assert_contains '^FAIL Pi executable: install pi on the host' "$root/missing-tools-doctor.out"
assert_contains '^FAIL Namespaces: install (bwrap|bash) on the host' "$root/missing-tools-doctor.out"
if env "${common_env[@]}" TMPDIR="$root" bwrap --unshare-user --disable-userns --unshare-pid \
  --ro-bind / / --bind "$root" "$root" --proc /proc --dev /dev \
  "$SLOPBOX" doctor --workspace "$workspace" >"$root/blocked-namespaces-doctor.out" 2>&1; then
  echo "doctor accepted blocked user namespaces" >&2
  exit 1
fi
assert_contains '^FAIL Namespaces: native namespace probe failed;' "$root/blocked-namespaces-doctor.out"
assert_contains '^OK +Pi executable:' "$root/blocked-namespaces-doctor.out"
if [[ -x /run/current-system/sw/bin/nix ]]; then
  printf 'throw "SLOPBOX_E2E_FLAKE_MUST_NOT_BE_EVALUATED"\n' >"$workspace/flake.nix"
  slopbox doctor --workspace "$workspace" --profile contained >"$root/flake-doctor.out"
  assert_contains '^OK +Development environment: project flake present;' "$root/flake-doctor.out"
  assert_not_contains 'SLOPBOX_E2E_FLAKE_MUST_NOT_BE_EVALUATED' "$root/flake-doctor.out"
  test ! -e "$workspace/flake.lock"
  rm "$workspace/flake.nix"
fi

echo "e2e: default launch and host-owned project setup"
setup_workspace="$root/setup-workspace"
mkdir "$setup_workspace"
if (cd "$setup_workspace" && slopbox </dev/null) >"$root/uninitialized.out" 2>"$root/uninitialized.err"; then
  echo "unconfigured noninteractive launch unexpectedly succeeded" >&2
  exit 1
fi
assert_contains 'project setup needs a host terminal' "$root/uninitialized.err"
test ! -e "$host_home/config/slopbox/projects"
if slopbox init --workspace "$setup_workspace" --changes staged </dev/null >"$root/unconfirmed.out" 2>"$root/unconfirmed.err"; then
  echo "noninteractive setup did not require explicit confirmation" >&2
  exit 1
fi
test ! -e "$host_home/config/slopbox/projects"
cat >"$setup_workspace/.slopbox.toml" <<'EOF'
[policy]
network = "none"
credentials = "none"
harness = "none"
EOF
slopbox init --workspace "$setup_workspace" --changes staged --yes >"$root/init.out" 2>"$root/init.err"
assert_contains '^Changes +Kept separate;' "$root/init.err"
assert_contains '^Saved ' "$root/init.err"
setup_config=$(find "$host_home/config/slopbox/projects" -name '*.toml')
test -f "$setup_config"
test "$(stat -c %a "$setup_config")" = 600
cmp "$host_home/config/slopbox/config.base.toml" "$host_home/config/slopbox/config.toml"
rm "$setup_workspace/.slopbox.toml"
slopbox policy --workspace "$setup_workspace" >"$root/saved-policy.out"
assert_contains '^workspace: staged$' "$root/saved-policy.out"
assert_contains '^network: none$' "$root/saved-policy.out"
assert_contains '^credentials: none$' "$root/saved-policy.out"
assert_contains '^harness: none$' "$root/saved-policy.out"
slopbox doctor --workspace "$setup_workspace" >"$root/saved-policy-doctor.out"
assert_contains '^Setup: saved host-side policy applies$' "$root/saved-policy-doctor.out"
assert_contains '^OK +Model configuration: disabled by policy$' "$root/saved-policy-doctor.out"
cp "$setup_config" "$root/saved-launch.toml"
(cd "$setup_workspace" && pi_rpc "$root/default-launch" "$SLOPBOX" -- --mode rpc --no-session) <<'EOF'
{"id":"default","type":"bash","command":"test ! -S /run/slopbox-host/model/gateway.sock && test ! -S /run/slopbox-host/general/gateway.sock && printf staged >default-change && printf SLOPBOX_E2E_DEFAULT_LAUNCH"}
EOF
assert_contains 'SLOPBOX_E2E_DEFAULT_LAUNCH' "$root/default-launch.out"
assert_contains '"command":"bash","success":true' "$root/default-launch.out"
assert_not_contains 'Project       ' "$root/default-launch.out"
assert_not_contains 'Save this access policy' "$root/default-launch.err"
test ! -e "$setup_workspace/default-change"
setup_stage=$(slopbox stage list --workspace "$setup_workspace")
setup_stage_id=$(printf '%s\n' "$setup_stage" | awk '{ print $1 }')
setup_stage_path=$(printf '%s\n' "$setup_stage" | awk '{ print $2 }')
test "$(cat "$setup_stage_path/default-change")" = staged
slopbox stage discard --workspace "$setup_workspace" "$setup_stage_id" >/dev/null
cmp "$setup_config" "$root/saved-launch.toml"

slopbox init --workspace "$setup_workspace" --changes read-only --no-host-pi-resources --yes >"$root/reinit.out" 2>"$root/reinit.err"
(cd "$setup_workspace" && pi_rpc "$root/default-readonly" "$SLOPBOX" -- --mode rpc --no-session) <<'EOF'
{"id":"readonly","type":"bash","command":"if printf leaked >readonly-leak; then exit 1; fi; printf SLOPBOX_E2E_DEFAULT_READ_ONLY"}
EOF
assert_contains 'SLOPBOX_E2E_DEFAULT_READ_ONLY' "$root/default-readonly.out"
test ! -e "$setup_workspace/readonly-leak"
cp "$setup_config" "$root/saved-readonly.toml"
cat >"$setup_workspace/.slopbox.toml" <<'EOF'
[policy]
workspace = "read-only"
EOF
if slopbox init --workspace "$setup_workspace" --changes live --yes >"$root/wider-init.out" 2>"$root/wider-init.err"; then
  echo "setup exceeded the project policy ceiling" >&2
  exit 1
fi
assert_contains 'exceeds the current workspace=read-only policy' "$root/wider-init.err"
cmp "$setup_config" "$root/saved-readonly.toml"
rm "$setup_workspace/.slopbox.toml"
if slopbox run --workspace "$setup_workspace" --dev-env none -- sh -c 'printf leaked >run-leak' >"$root/saved-run.out" 2>"$root/saved-run.err"; then
  echo "explicit run ignored saved project policy" >&2
  exit 1
fi
test ! -e "$setup_workspace/run-leak"

unauthed_workspace="$root/unauthed-workspace"
mkdir "$unauthed_workspace"
slopbox init --workspace "$unauthed_workspace" --changes live --no-host-pi-resources --yes >"$root/unauthed-init.out" 2>"$root/unauthed-init.err"
if (cd "$unauthed_workspace" && env "${common_env[@]}" OPENROUTER_API_KEY= XDG_DATA_HOME="$root/missing-data" "$SLOPBOX" </dev/null) >"$root/unauthed.out" 2>"$root/unauthed.err"; then
  echo "default launch failed to diagnose missing model access" >&2
  exit 1
fi
assert_contains 'no model account is configured' "$root/unauthed.err"
assert_contains 'slopbox auth login openai-codex' "$root/unauthed.err"
test ! -e "$root/missing-data"

echo "e2e: first-run terminal confirmation"
interactive_workspace="$root/interactive-workspace"
mkdir "$interactive_workspace"
project_count=$(find "$host_home/config/slopbox/projects" -name '*.toml' | wc -l)
if printf '\nn\n' | (cd "$interactive_workspace" && env "${common_env[@]}" SLOPBOX="$SLOPBOX" \
  timeout 15s script -q -e -c 'exec "$SLOPBOX" -- --version' /dev/null) >"$root/cancelled-setup.out" 2>&1; then
  echo "first-run setup accepted a negative confirmation" >&2
  exit 1
fi
assert_contains 'How should changes work' "$root/cancelled-setup.out"
assert_contains 'setup cancelled; no configuration changed' "$root/cancelled-setup.out"
test "$(find "$host_home/config/slopbox/projects" -name '*.toml' | wc -l)" = "$project_count"
printf '2\ny\n' | (cd "$interactive_workspace" && env "${common_env[@]}" SLOPBOX="$SLOPBOX" \
  timeout 15s script -q -e -c 'exec "$SLOPBOX" -- --version' /dev/null) >"$root/interactive-setup.out" 2>&1
assert_contains 'How should changes work' "$root/interactive-setup.out"
assert_contains 'Save this access policy for Pi' "$root/interactive-setup.out"
assert_contains 'Saved ' "$root/interactive-setup.out"
slopbox policy --workspace "$interactive_workspace" >"$root/interactive-policy.out"
assert_contains '^workspace: staged$' "$root/interactive-policy.out"
interactive_stage_id=$(slopbox stage list --workspace "$interactive_workspace" | awk '{ print $1 }')
slopbox stage discard --workspace "$interactive_workspace" "$interactive_stage_id" >/dev/null

echo "e2e: host credential inventory"
if [[ $(slopbox auth list) != "openai-codex" ]]; then
  echo "expected openai-codex in credential inventory" >&2
  exit 1
fi
if [[ $(slopbox auth status openai-codex) != "openai-codex: configured" ]]; then
  echo "expected openai-codex to be configured" >&2
  exit 1
fi
slopbox policy --workspace "$workspace" >"$root/policy.out"
assert_contains '^profile: developer$' "$root/policy.out"
assert_contains '^status: implemented$' "$root/policy.out"
assert_contains '^workspace: live$' "$root/policy.out"
assert_contains '^network: allowlist$' "$root/policy.out"
assert_contains '^runtime: host$' "$root/policy.out"
assert_contains '^harness: trusted$' "$root/policy.out"
slopbox policy --workspace "$workspace" --no-host-pi-resources >"$root/strict-policy.out"
assert_contains '^harness: none$' "$root/strict-policy.out"

if slopbox run --workspace "$workspace" --profile contained --dry-run -- /bin/true >"$root/contained.out" 2>"$root/contained.err"; then
  echo "contained dry run unexpectedly resolved a project closure" >&2
  exit 1
fi
assert_contains '--dry-run cannot resolve a project runtime closure' "$root/contained.err"

echo "e2e: gateway planes and inner tool modes"
slopbox run --workspace "$workspace" --dev-env none -- /bin/sh -eu -c '
  test -S /run/slopbox-host/general/gateway.sock
  test -S /run/slopbox-host/model/gateway.sock
  test -z "${GIT_CONFIG_GLOBAL-}"
  test -f /run/slopbox-pi-agent/extensions/slopbox.ts
  test -f /run/slopbox-host-pi/extensions/e2e-host.ts
  test "$(/home/slopbox/.pi/agent/bin/e2e-helper)" = SLOPBOX_E2E_HOST_HELPER
  test -f /home/slopbox/.pi/agent/extensions/e2e-host.ts
  test -f /run/slopbox-host-pi/skills/e2e-host-skill/SKILL.md
  test -f /run/slopbox-host-pi/prompts/e2e-host.md
  test -f /run/slopbox-host-pi/npm/node_modules/e2e-host-package/index.ts
  test -f /home/slopbox/.pi/agent/npm/node_modules/e2e-host-package/index.ts
  test "$(cat /home/slopbox/.cache/e2e-resource/data.txt)" = "mounted resource"
  if { printf modified >/home/slopbox/.cache/e2e-resource/data.txt; } 2>/dev/null; then
    exit 22
  fi
  test "$(cat /home/slopbox/.cache/e2e-overlay/data.txt)" = "overlay lower"
  printf "overlay modified\n" >/home/slopbox/.cache/e2e-overlay/data.txt
  test "$(cat /home/slopbox/.cache/e2e-overlay/data.txt)" = "overlay modified"
  test ! -f /run/slopbox-pi-agent/models.json
  test ! -f /run/slopbox-pi-agent/auth.json
  test ! -f /run/slopbox-pi-agent/settings.json
  rg -q "openai-codex" /run/slopbox-pi-agent/extensions/slopbox.ts
  ! rg -q "slopbox-e2e-codex-canary" /run/slopbox-pi-agent
  test "$OPENROUTER_API_KEY" = slopbox:openrouter
  test "$SLOPBOX_CODEX_ENABLED" = 1
  test "$SLOPBOX_MODEL_PROXY_PORT" = 39081
  test -z "${PI_OFFLINE-}"
  rg -q "\"transport\": \"sse\"" "$PI_CODING_AGENT_DIR/settings.json"
  rg -q "\"defaultProjectTrust\": \"never\"" "$PI_CODING_AGENT_DIR/settings.json"
  rg -q "\"defaultThinkingLevel\": \"high\"" "$PI_CODING_AGENT_DIR/settings.json"
  rg -q "\"quietStartup\": true" "$PI_CODING_AGENT_DIR/settings.json"
  ! rg -q "externalEditor|httpProxy|extensions|packages|must-not" "$PI_CODING_AGENT_DIR/settings.json"
  rg -q "SLOPBOX_E2E_HOST_AGENTS" "$PI_CODING_AGENT_DIR/AGENTS.md"
  if { printf modified >"$PI_CODING_AGENT_DIR/AGENTS.md"; } 2>/dev/null; then
    exit 21
  fi
  test "$(curl -sS -o /tmp/general-model -w "%{http_code}" --noproxy "*" -X POST http://127.0.0.1:39080/openrouter/api/v1/chat/completions)" = 404
  test "$(curl -sS -o /tmp/model-connect -w "%{http_code}" --noproxy "*" -X CONNECT http://127.0.0.1:39081/example.com:443)" = 404

  slopbox tool-run --network none -- /bin/sh -eu -c '\''
    test ! -S /run/slopbox-host/general/gateway.sock
    test ! -S /run/slopbox-host/model/gateway.sock
    test ! -f /run/slopbox-pi-agent/extensions/slopbox.ts
    test -z "${OPENROUTER_API_KEY-}"
    test -z "${HTTP_PROXY-}"
    test -z "${PI_CODING_AGENT_DIR-}"
    printf persisted >"$HOME/e2e-cache-marker"
    printf workspace >.e2e-workspace-write
  '\''

  slopbox tool-run --network general -- /bin/sh -eu -c '\''
    test -S /run/slopbox-host/general/gateway.sock
    test ! -S /run/slopbox-host/model/gateway.sock
    test -z "${OPENROUTER_API_KEY-}"
    test -n "${HTTP_PROXY-}"
    test "$(cat "$HOME/e2e-cache-marker")" = persisted
    test "$(curl -sS -o /tmp/denials -w "%{http_code}" --noproxy "*" http://127.0.0.1:39080/denials)" = 200
    if curl -sS --connect-timeout 1 --noproxy "*" http://127.0.0.1:39081/openrouter/api/v1/chat/completions >/dev/null 2>&1; then
      exit 20
    fi
  '\''
'
test "$(cat "$workspace/.e2e-workspace-write")" = workspace
test "$(cat "$host_home/.cache/e2e-overlay/data.txt")" = "overlay lower"
rm "$workspace/.e2e-workspace-write"
assert_not_contains "$canary" "$workspace" "$host_home/data/slopbox"
assert_not_contains "$codex_canary" "$workspace" "$host_home/data/slopbox/boxes"

echo "e2e: concurrent sessions keep mounts, settings, and approvals independent"
cat >"$workspace/.e2e-concurrent.sh" <<'EOF'
set -eu
label=$1
if test "$label" = first; then
  rg -q SLOPBOX_E2E_HOST_AGENTS "$PI_CODING_AGENT_DIR/AGENTS.md"
  printf history >"$PI_CODING_AGENT_DIR/sessions/.e2e-history"
  printf cache >"$HOME/.cache/e2e-agent-cache"
  printf "{}\n" >"$PI_CODING_AGENT_DIR/models-store.json"
else
  test ! -s "$PI_CODING_AGENT_DIR/AGENTS.md"
  test "$(cat "$PI_CODING_AGENT_DIR/sessions/.e2e-history")" = history
  test "$(cat "$HOME/.cache/e2e-agent-cache")" = cache
  test "$(cat "$PI_CODING_AGENT_DIR/models-store.json")" = "{}"
fi
agent_inode=$(stat -c %i /run/slopbox-pi-agent)
printf '%s\n' "$label" >"$PI_CODING_AGENT_DIR/settings.json"
printf '%s\n' "$label" >"$PI_CODING_AGENT_DIR/auth.json"
slopbox tool-run --network none -- true
curl -sS --connect-timeout 2 "https://$label.slopbox-concurrent.invalid/" >/dev/null 2>&1 || true
slopbox denials >".e2e-$label-denials"
: >".e2e-$label-ready"
for phase in check stop; do
  count=0
  until test -e ".e2e-$label-$phase"; do
    count=$((count + 1))
    test "$count" -lt 400
    sleep 0.05
  done
  slopbox tool-run --network none -- true
  test "$(stat -c %i /run/slopbox-pi-agent)" = "$agent_inode"
  test -f /run/slopbox-pi-agent/extensions/slopbox.ts
  test "$(cat "$PI_CODING_AGENT_DIR/settings.json")" = "$label"
  test "$(cat "$PI_CODING_AGENT_DIR/auth.json")" = "$label"
  if test "$label" = first; then
    rg -q SLOPBOX_E2E_HOST_AGENTS "$PI_CODING_AGENT_DIR/AGENTS.md"
  else
    test ! -s "$PI_CODING_AGENT_DIR/AGENTS.md"
  fi
  slopbox tool-run --network general -- /bin/sh -eu -c '
    test -S /run/slopbox-host/general/gateway.sock
    test ! -S /run/slopbox-host/model/gateway.sock
    test ! -f /run/slopbox-pi-agent/extensions/slopbox.ts
    test ! -e "$HOME/.pi/agent/settings.json"
    test ! -e "$HOME/.pi/agent/auth.json"
  '
  : >".e2e-$label-$phase-done"
done
EOF
slopbox run --workspace "$workspace" --dev-env none -- /bin/sh .e2e-concurrent.sh first \
  >"$root/first.out" 2>"$root/first.err" &
active_pid=$!
wait_for_file "$workspace/.e2e-first-ready" || { cat "$root/first.err" >&2; exit 1; }
slopbox run --workspace "$workspace" --dev-env none --no-host-pi-resources -- /bin/sh .e2e-concurrent.sh second \
  >"$root/second.out" 2>"$root/second.err" &
second_pid=$!
wait_for_file "$workspace/.e2e-second-ready" || { cat "$root/second.err" >&2; exit 1; }
: >"$workspace/.e2e-first-check"
: >"$workspace/.e2e-second-check"
wait_for_file "$workspace/.e2e-first-check-done" || { cat "$root/first.err" >&2; exit 1; }
wait_for_file "$workspace/.e2e-second-check-done" || { cat "$root/second.err" >&2; exit 1; }

first_request=$(awk '$2 == "CONNECT" { print $1; exit }' "$workspace/.e2e-first-denials")
second_request=$(awk '$2 == "CONNECT" { print $1; exit }' "$workspace/.e2e-second-denials")
test -n "$first_request"
test -n "$second_request"
slopbox network events --workspace "$workspace" >"$root/concurrent-events.out"
assert_contains "$first_request" "$root/concurrent-events.out"
assert_contains "$second_request" "$root/concurrent-events.out"
slopbox network approve "$first_request" --session --workspace "$workspace" >/dev/null
slopbox network approve "$second_request" --session --workspace "$workspace" >/dev/null
: >"$workspace/.e2e-second-stop"
wait "$second_pid"
second_pid=
slopbox network events --workspace "$workspace" >"$root/remaining-events.out"
assert_contains "^$first_request .*active=true" "$root/remaining-events.out"
assert_contains "^$second_request .*active=false" "$root/remaining-events.out"
slopbox network approve "$first_request" --session --workspace "$workspace" >/dev/null
if slopbox network approve "$second_request" --session --workspace "$workspace" >"$root/expired.out" 2>"$root/expired.err"; then
  echo "ended session still accepts approvals" >&2
  exit 1
fi
assert_contains 'session is no longer running' "$root/expired.err"
cat >"$workspace/.slopbox.toml" <<'EOF'
[policy]
network = "none"
credentials = "none"
harness = "none"
EOF
slopbox run --workspace "$workspace" --dev-env none -- sh -eu -c '
  test "$(cat "$PI_CODING_AGENT_DIR/sessions/.e2e-history")" = history
  test "$(cat "$HOME/.cache/e2e-agent-cache")" = cache
  test "$(cat "$PI_CODING_AGENT_DIR/models-store.json")" = "{}"
  slopbox tool-run --network none -- true
  rm "$PI_CODING_AGENT_DIR/sessions/.e2e-history" "$HOME/.cache/e2e-agent-cache"
'
rm "$workspace/.slopbox.toml"
: >"$workspace/.e2e-first-stop"
wait "$active_pid"
active_pid=
rm -f "$workspace"/.e2e-*
if find "$host_home/data/slopbox/boxes" -maxdepth 2 -name 'run-*' | rg -q .; then
  echo "completed sessions left generated runtime files behind" >&2
  exit 1
fi
if slopbox run --workspace "$workspace" --dev-env flake -- /bin/true >"$root/failed-setup.out" 2>"$root/failed-setup.err"; then
  echo "flake setup unexpectedly succeeded without flake.nix" >&2
  exit 1
fi
assert_contains 'requires flake.nix' "$root/failed-setup.err"
if find "$host_home/data/slopbox/boxes" -maxdepth 2 -name 'run-*' | rg -q .; then
  echo "failed setup left generated runtime files behind" >&2
  exit 1
fi

echo "e2e: denial, session approval, and project approval"
rm -f "$workspace/.e2e-ready" "$workspace/.e2e-approved"
slopbox run --workspace "$workspace" --dev-env none -- /bin/sh -eu -c '
  curl -sS --connect-timeout 2 https://packages.slopbox-e2e.invalid/ >/dev/null 2>&1 || true
  : >.e2e-ready
  count=0
  until test -e .e2e-approved; do
    count=$((count + 1))
    test "$count" -lt 100
    sleep 0.05
  done
  curl -sS --connect-timeout 2 https://packages.slopbox-e2e.invalid/ >/dev/null 2>&1 || true
  slopbox denials >.e2e-guest-denials
' >"$root/approval-session.out" 2>"$root/approval-session.err" &
active_pid=$!
wait_for_file "$workspace/.e2e-ready"

events=$(slopbox network events --workspace "$workspace")
request_id=$(printf '%s\n' "$events" | awk '$2 == "CONNECT" && $3 == "packages.slopbox-e2e.invalid:443" { print $1; exit }')
if [[ -z "$request_id" ]]; then
  echo "denied package destination did not produce an event" >&2
  exit 1
fi
slopbox network approve "$request_id" --session --workspace "$workspace" >/dev/null
: >"$workspace/.e2e-approved"
wait "$active_pid"
active_pid=

assert_contains 'packages\.slopbox-e2e\.invalid:443' "$workspace/.e2e-guest-denials"
assert_contains 'DNS resolution failed' "$workspace/.e2e-guest-denials"
slopbox network approve "$request_id" --project --workspace "$workspace" >/dev/null
slopbox run --workspace "$workspace" --dev-env none -- /bin/sh -eu -c '
  curl -sS --connect-timeout 2 https://packages.slopbox-e2e.invalid/ >/dev/null 2>&1 || true
  slopbox denials >.e2e-project-denials
'
assert_contains 'DNS resolution failed' "$workspace/.e2e-project-denials"
if rg -q 'no matching allow rule' "$workspace/.e2e-project-denials"; then
  echo "project approval was not applied to the next session" >&2
  exit 1
fi
slopbox network approvals --workspace "$workspace" >"$root/project-rules.out"
project_rule=$(awk '$1 == "active" && $3 == "project" && $4 == "packages.slopbox-e2e.invalid:443" {print $2}' "$root/project-rules.out")
test -n "$project_rule"
slopbox network revoke "$project_rule" --workspace "$workspace" >/dev/null
slopbox run --workspace "$workspace" --dev-env none -- /bin/sh -eu -c '
  curl -sS --connect-timeout 2 https://packages.slopbox-e2e.invalid/ >/dev/null 2>&1 || true
  slopbox denials >.e2e-project-revoked
'
assert_contains 'no matching allow rule' "$workspace/.e2e-project-revoked"
rm -f "$workspace"/.e2e-*

echo "e2e: revocable rules, structured events, and host-only mutation"
slopbox network events --workspace "$workspace" --follow --json >"$root/follow-events.out" 2>"$root/follow-events.err" &
follow_pid=$!
slopbox run --workspace "$workspace" --dev-env none -- /bin/sh -eu -c '
  curl -sS --max-time 10 http://revocable.slopbox-e2e.invalid/ >.e2e-initial
  : >.e2e-ready
  count=0
  until test -e .e2e-approved; do
    count=$((count + 1)); test "$count" -lt 100; sleep 0.05
  done
  rule=$(cat .e2e-rule)
  request=$(awk '\''$1 == "request:" {print $2}'\'' .e2e-initial)
  if slopbox network revoke "$rule" >.e2e-guest-revoke 2>&1; then exit 1; fi
  if slopbox network approve "$request" --project >.e2e-guest-approve 2>&1; then exit 1; fi
  curl -sS --max-time 10 http://revocable.slopbox-e2e.invalid/ >.e2e-permitted
  : >.e2e-attempted
  count=0
  until test -e .e2e-revoked; do
    count=$((count + 1)); test "$count" -lt 100; sleep 0.05
  done
  curl -sS --max-time 10 http://revocable.slopbox-e2e.invalid/ >.e2e-after-revoke
' >"$root/revoke-session.out" 2>"$root/revoke-session.err" &
active_pid=$!
wait_for_file "$workspace/.e2e-ready"
assert_contains 'no matching allow rule' "$workspace/.e2e-initial"
request_id=$(awk '$1 == "request:" { print $2 }' "$workspace/.e2e-initial")
slopbox network approve "$request_id" --workspace "$workspace" >"$root/approved-rule.out"
assert_contains '^approved rule-[a-f0-9]+ session revocable.slopbox-e2e.invalid:80 ' "$root/approved-rule.out"
assert_contains 'retry the operation' "$root/approved-rule.out"
rule_id=$(awk '{print $2}' "$root/approved-rule.out")
printf '%s\n' "$rule_id" >"$workspace/.e2e-rule"
: >"$workspace/.e2e-approved"
wait_for_file "$workspace/.e2e-attempted"
assert_contains 'DNS resolution failed' "$workspace/.e2e-permitted"
slopbox network approvals --workspace "$workspace" >"$root/live-rules.out"
assert_contains "^active $rule_id session " "$root/live-rules.out"
slopbox network revoke "$rule_id" --workspace "$workspace" >"$root/revoked-rule.out"
assert_contains '^revoked rule-' "$root/revoked-rule.out"
: >"$workspace/.e2e-revoked"
wait "$active_pid"
active_pid=
assert_contains 'no matching allow rule' "$workspace/.e2e-after-revoke"
slopbox network approvals --workspace "$workspace" >"$root/revoked-rules.out"
assert_contains "^revoked $rule_id session " "$root/revoked-rules.out"
assert_contains '^expired rule-' "$root/revoked-rules.out"
slopbox network approvals --workspace "$workspace" --json >"$root/rules.json"
assert_contains '"revoked_at_ms":[0-9]+' "$root/rules.json"
slopbox network events --workspace "$workspace" --json >"$root/events.jsonl"
assert_contains '"session_active":false' "$root/events.jsonl"
assert_contains '"created_at_ms":[0-9]+' "$root/events.jsonl"
assert_contains '"project":"'"$workspace"'"' "$root/events.jsonl"
for _ in $(seq 1 100); do
  if rg -q -F "\"id\":\"$request_id\"" "$root/follow-events.out"; then break; fi
  sleep 0.05
done
test "$(rg -c -F "\"id\":\"$request_id\"" "$root/follow-events.out")" = 1
kill "$follow_pid"
wait "$follow_pid" 2>/dev/null || true
follow_pid=
test ! -s "$root/follow-events.err"
assert_not_contains "$canary" "$root/events.jsonl" "$root/rules.json"
assert_not_contains "$codex_canary" "$root/events.jsonl" "$root/rules.json"
rm -f "$workspace"/.e2e-*

echo "e2e: host-controlled terminal approvals"
if slopbox run --approval-view --workspace "$workspace" --dev-env none -- /bin/true </dev/null >"$root/no-tty.out" 2>"$root/no-tty.err"; then
  echo "approval view accepted non-terminal input" >&2
  exit 1
fi
assert_contains 'approval view requires a host terminal' "$root/no-tty.err"
env "${common_env[@]}" node "$TERMINAL_TESTS_DIR/approval-view.mjs" "$SLOPBOX" "$root"
env "${common_env[@]}" node "$TERMINAL_TESTS_DIR/terminal-lifecycle.mjs" "$SLOPBOX" "$root"
env "${common_env[@]}" node "$TERMINAL_TESTS_DIR/terminal-resize.mjs" "$SLOPBOX" "$root"
env "${common_env[@]}" node "$TERMINAL_TESTS_DIR/clipboard.mjs" "$SLOPBOX" "$root"

echo "e2e: Pi adapter, project trust, and network selection"
slopbox run --workspace "$workspace" --dev-env none -- pi --list-models openai-codex >"$root/pi-codex-models.out"
assert_contains 'openai-codex' "$root/pi-codex-models.out"

pi_rpc "$root/pi-general" "$SLOPBOX" run --workspace "$workspace" --dev-env none -- pi --mode rpc --no-session <<'EOF'
{"id":"commands","type":"get_commands"}
{"id":"general","type":"bash","command":"test -S /run/slopbox-host/general/gateway.sock && test ! -S /run/slopbox-host/model/gateway.sock && test ! -f /run/slopbox-pi-agent/extensions/slopbox.ts && test -z \"${OPENROUTER_API_KEY-}\" && test -z \"${PI_CODING_AGENT_DIR-}\" && test -n \"${HTTP_PROXY-}\" && printf SLOPBOX_E2E_PI_GENERAL"}
EOF
assert_contains 'SLOPBOX_E2E_PI_GENERAL' "$root/pi-general.out"
assert_contains '"command":"bash","success":true' "$root/pi-general.out"
assert_contains 'slopbox_e2e_host_extension_loaded' "$root/pi-general.out"
assert_contains 'slopbox_e2e_package_extension_loaded' "$root/pi-general.out"
assert_contains 'trusted Pi extension: /run/slopbox-host-pi/extensions/e2e-host.ts' "$root/pi-general.err"
if rg -q 'slopbox_e2e_project_extension_loaded' "$root/pi-general.out" "$root/pi-general.err"; then
  echo "project-local Pi extension was loaded" >&2
  exit 1
fi

cat >"$workspace/.slopbox.toml" <<'EOF'
[policy]
harness = "data"
EOF
pi_rpc "$root/pi-data-resources" "$SLOPBOX" run --workspace "$workspace" --dev-env none -- pi --mode rpc --no-session <<'EOF'
{"id":"commands","type":"get_commands"}
EOF
assert_not_contains 'slopbox_e2e_host_extension_loaded' "$root/pi-data-resources.out" "$root/pi-data-resources.err"
assert_not_contains 'slopbox_e2e_package_extension_loaded' "$root/pi-data-resources.out" "$root/pi-data-resources.err"
assert_contains 'trusted host Pi resources: 0 extension\(s\), 1 skill path\(s\), 1 prompt path\(s\), 0 theme path\(s\)' "$root/pi-data-resources.err"
rm "$workspace/.slopbox.toml"

pi_rpc "$root/pi-no-host-resources" "$SLOPBOX" run --workspace "$workspace" --dev-env none --no-host-pi-resources -- pi --mode rpc --no-session <<'EOF'
{"id":"commands","type":"get_commands"}
EOF
assert_not_contains 'slopbox_e2e_host_extension_loaded' "$root/pi-no-host-resources.out" "$root/pi-no-host-resources.err"
assert_not_contains 'slopbox_e2e_package_extension_loaded' "$root/pi-no-host-resources.out" "$root/pi-no-host-resources.err"
assert_not_contains 'SLOPBOX_E2E_HOST_AGENTS' "$host_home/data/slopbox/boxes"

pi_rpc "$root/pi-offline" "$SLOPBOX" run --workspace "$workspace" --dev-env none --tool-network none -- pi --mode rpc --no-session <<'EOF'
{"id":"offline","type":"bash","command":"test ! -S /run/slopbox-host/general/gateway.sock && test ! -S /run/slopbox-host/model/gateway.sock && test -z \"${HTTP_PROXY-}\" && printf SLOPBOX_E2E_PI_OFFLINE"}
EOF
assert_contains 'SLOPBOX_E2E_PI_OFFLINE' "$root/pi-offline.out"
assert_contains '"command":"bash","success":true' "$root/pi-offline.out"

echo "e2e: Pi shell cancellation"
rm -f "$workspace/.e2e-command-started" "$workspace/.e2e-abort-leak"
mkfifo "$root/pi-abort.in"
slopbox run --workspace "$workspace" --dev-env none -- pi --mode rpc --no-session \
  <"$root/pi-abort.in" >"$root/pi-abort.out" 2>"$root/pi-abort.err" &
active_pid=$!
exec 9>"$root/pi-abort.in"
printf '%s\n' '{"id":"long","type":"bash","command":": >.e2e-command-started; sleep 2; printf leaked >.e2e-abort-leak"}' >&9
wait_for_file "$workspace/.e2e-command-started"
printf '%s\n' '{"id":"abort","type":"abort_bash"}' >&9
for _ in $(seq 1 100); do
  if rg -q '"command":"abort_bash","success":true' "$root/pi-abort.out"; then
    break
  fi
  sleep 0.05
done
assert_contains '"command":"abort_bash","success":true' "$root/pi-abort.out"
exec 9>&-
wait "$active_pid"
active_pid=
sleep 2.2
if [[ -e "$workspace/.e2e-abort-leak" ]]; then
  echo "aborted Pi shell process continued running" >&2
  exit 1
fi
rm -f "$workspace/.e2e-command-started"

cat >"$workspace/.slopbox.toml" <<'EOF'
[policy]
credentials = "none"
harness = "none"
EOF
slopbox policy --workspace "$workspace" >"$root/no-credentials-policy.out"
assert_contains '^status: implemented$' "$root/no-credentials-policy.out"
assert_contains '^credentials: none$' "$root/no-credentials-policy.out"
slopbox run --workspace "$workspace" --dev-env none -- sh -eu -c '
  test -n "${HTTP_PROXY-}"
  test -n "${SLOPBOX_PROXY_PORT-}"
  test -z "${SLOPBOX_MODEL_PROXY_PORT-}"
  test -z "${OPENROUTER_API_KEY-}"
  test -z "${SLOPBOX_CODEX_ENABLED-}"
  test -S /run/slopbox-host/general/gateway.sock
  test ! -S /run/slopbox-host/model/gateway.sock
'

cat >"$workspace/.slopbox.toml" <<'EOF'
[policy]
network = "none"
harness = "none"
EOF
slopbox policy --workspace "$workspace" >"$root/no-network-policy.out"
assert_contains '^status: implemented$' "$root/no-network-policy.out"
assert_contains '^network: none$' "$root/no-network-policy.out"
slopbox run --workspace "$workspace" --dev-env none -- sh -eu -c '
  test -z "${HTTP_PROXY-}"
  test -z "${SLOPBOX_PROXY_PORT-}"
  test ! -S /run/slopbox-host/general/gateway.sock
  test -S /run/slopbox-host/model/gateway.sock
  test -n "${SLOPBOX_MODEL_PROXY_PORT-}"
  test -n "${OPENROUTER_API_KEY-}"
  if curl --noproxy "*" --connect-timeout 1 http://127.0.0.1:39080/denials >/dev/null 2>&1; then
    exit 1
  fi
'

cat >"$workspace/.slopbox.toml" <<'EOF'
[policy]
network = "none"
credentials = "none"
harness = "none"
EOF
slopbox run --workspace "$workspace" --dev-env none -- sh -eu -c '
  test -z "${HTTP_PROXY-}"
  test -z "${SLOPBOX_PROXY_PORT-}"
  test -z "${SLOPBOX_MODEL_PROXY_PORT-}"
  test -z "${OPENROUTER_API_KEY-}"
  test -z "${SLOPBOX_CODEX_ENABLED-}"
  test ! -S /run/slopbox-host/general/gateway.sock
  test ! -S /run/slopbox-host/model/gateway.sock
'
rm "$workspace/.slopbox.toml"

printf 'original\n' >"$workspace/staged-file"
cat >"$workspace/.slopbox.toml" <<'EOF'
[policy]
workspace = "read-only"
harness = "none"
EOF
slopbox policy --workspace "$workspace" >"$root/project-policy.out"
assert_contains '^workspace: read-only$' "$root/project-policy.out"
assert_contains '^harness: none$' "$root/project-policy.out"
if slopbox run --workspace "$workspace" --dev-env none -- sh -c 'printf modified >staged-file' 2>"$root/read-only.err"; then
  echo "read-only workspace was writable" >&2
  exit 1
fi
assert_contains '^original$' "$workspace/staged-file"

cat >"$workspace/.slopbox.toml" <<'EOF'
[policy]
workspace = "staged"
harness = "none"
EOF
slopbox run --workspace "$workspace" --dev-env none -- sh -c 'printf staged >staged-file; chmod 755 staged-file; ln -s "$1" staged-link; rm input.txt; mkdir staged-directory; printf new >staged-directory/file' sh "$canary" 2>"$root/staged.err"
assert_contains '^original$' "$workspace/staged-file"
staged_workspace=$(sed -n 's/^slopbox: staged workspace retained at //p' "$root/staged.err" | tail -n 1)
if [[ -z "$staged_workspace" || $(cat "$staged_workspace/staged-file") != "staged" ]]; then
  echo "staged workspace was not retained with the sandbox changes" >&2
  exit 1
fi
stage_root=$(dirname "$staged_workspace")
stage_id=$(basename "$stage_root")
slopbox stage list --workspace "$workspace" >"$root/stages.out"
assert_contains "^${stage_id}[[:space:]]+${staged_workspace}$" "$root/stages.out"
slopbox stage diff --workspace "$workspace" "$stage_id" >"$root/stage.diff"
assert_contains '^-original$' "$root/stage.diff"
assert_contains '^\+staged$' "$root/stage.diff"
assert_contains '^mode staged-file: 0644 -> 0755$' "$root/stage.diff"
slopbox stage apply --workspace "$workspace" "$stage_id" >"$root/apply.out"
assert_contains "^applied ${stage_id}$" "$root/apply.out"
if [[ $(cat "$workspace/staged-file") != "staged" || ! -x "$workspace/staged-file" ]]; then
  echo "stage changes were not applied to the host workspace" >&2
  exit 1
fi
if [[ ! -L "$workspace/staged-link" || $(readlink "$workspace/staged-link") != "$canary" ]]; then
  echo "staged symlink was not applied as a symlink" >&2
  exit 1
fi
if [[ -e "$workspace/input.txt" || $(cat "$workspace/staged-directory/file") != "new" ]]; then
  echo "stage additions or deletions were not applied" >&2
  exit 1
fi
slopbox stage discard --workspace "$workspace" "$stage_id" >"$root/discard.out"
assert_contains "^discarded ${stage_id}$" "$root/discard.out"
if [[ -e "$stage_root" ]]; then
  echo "discarded stage still exists" >&2
  exit 1
fi

slopbox run --workspace "$workspace" --dev-env none -- sh -c 'printf agent-second >staged-file' 2>"$root/conflict-stage.err"
conflict_workspace=$(sed -n 's/^slopbox: staged workspace retained at //p' "$root/conflict-stage.err" | tail -n 1)
conflict_id=$(basename "$(dirname "$conflict_workspace")")
printf 'host-change' >"$workspace/staged-file"
if slopbox stage apply --workspace "$workspace" "$conflict_id" >"$root/conflict-apply.out" 2>"$root/conflict-apply.err"; then
  echo "stage apply ignored a concurrent host workspace change" >&2
  exit 1
fi
assert_contains 'host workspace changed after stage creation; refusing to apply' "$root/conflict-apply.err"
if [[ $(cat "$workspace/staged-file") != "host-change" ]]; then
  echo "conflicting stage apply modified the host workspace" >&2
  exit 1
fi
slopbox stage discard --workspace "$workspace" "$conflict_id" >/dev/null
rm "$workspace/.slopbox.toml" "$workspace/staged-file" "$workspace/staged-link"

cat >>"$host_home/config/slopbox/config.toml" <<EOF

[secrets.e2e_route]
source = "environment"
variable = "OPENROUTER_API_KEY"

[[http_routes]]
name = "e2e"
workspace = "$workspace"
upstream = "https://example.com/repository"
methods = ["GET"]
direct = true
authentication = { type = "bearer", secret = "e2e_route" }
EOF
slopbox run --workspace "$workspace" --dev-env none -- sh -eu -c '
  test -S /run/slopbox-host/authenticated-http/gateway.sock
  test -S /run/slopbox-host/authenticated-http/direct.sock
  test "$SLOPBOX_AUTHENTICATED_HTTP_BASE_URL" = http://127.0.0.1:39082
  test -z "${GH_TOKEN-}${GH_CONFIG_DIR-}"
  slopbox tool-run --network none -- sh -eu -c '\''
    test -S /run/slopbox-host/authenticated-http/gateway.sock
    test "$SLOPBOX_AUTHENTICATED_HTTP_BASE_URL" = http://127.0.0.1:39082
    test -z "${OPENROUTER_API_KEY-}"
    test "$GH_TOKEN" = slopbox-brokered-authentication
    test "$GH_CONFIG_DIR" = /run/slopbox/github
    test -r "$GH_CONFIG_DIR/config.yml"
    if { printf modified >"$GH_CONFIG_DIR/config.yml"; } 2>/dev/null; then exit 1; fi
    code=$(curl --silent --unix-socket /run/slopbox-host/authenticated-http/direct.sock --request POST --output /tmp/account-response --write-out "%{http_code}" http://example.com/repository)
    test "$code" = 405
  '\''
'
assert_not_contains "$canary" "$workspace" "$host_home/data/slopbox"

echo "e2e: ordinary Git commands use workspace-bound broker routes"
git_workspace="$root/git-workspace"
env GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1 git init --quiet --initial-branch=main "$git_workspace"
env GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1 git -C "$git_workspace" \
  -c user.name=Test -c user.email=test@example.com commit --quiet --no-gpg-sign --allow-empty -m initial
cat >>"$git_workspace/.git/config" <<'EOF'
[remote "origin"]
  url = ssh://git@forge.slopbox-e2e.invalid/org/repo.git
  pushurl = https://forge.slopbox-e2e.invalid/org/repo.git
  fetch = +refs/heads/*:refs/remotes/origin/*
[branch "main"]
  remote = origin
  merge = refs/heads/main
EOF
cp "$git_workspace/.git/config" "$root/original-gitconfig"
cat >>"$host_home/config/slopbox/config.toml" <<EOF

[[http_routes]]
name = "git"
workspace = "$git_workspace"
upstream = "https://forge.slopbox-e2e.invalid/org/repo.git"
methods = ["POST"]
authentication = { type = "bearer", secret = "e2e_route" }
git_urls = ["ssh://git@forge.slopbox-e2e.invalid/org/repo.git", "https://forge.slopbox-e2e.invalid/org/repo.git"]
EOF
cat >"$git_workspace/check-git.sh" <<'EOF'
set -eu
test "$GIT_CONFIG_GLOBAL" = /run/slopbox/gitconfig
test "$GIT_CONFIG_NOSYSTEM" = 1
test ! -e /run/slopbox/git-sign
test "$(git remote get-url origin)" = "$SLOPBOX_AUTHENTICATED_HTTP_BASE_URL/git"
test "$(git remote get-url --push origin)" = "$SLOPBOX_AUTHENTICATED_HTTP_BASE_URL/git"
test "$(git config --local --get remote.origin.url)" = ssh://git@forge.slopbox-e2e.invalid/org/repo.git
test "$(git ls-remote --get-url ssh://git@forge.slopbox-e2e.invalid/org/other.git)" = ssh://git@forge.slopbox-e2e.invalid/org/other.git
if { printf modified >"$GIT_CONFIG_GLOBAL"; } 2>/dev/null; then
  exit 1
fi
# GET is deliberately denied before upstream DNS or authentication is attempted.
if git fetch origin >/tmp/git.out 2>/tmp/git.err; then exit 1; fi
rg -q '405' /tmp/git.err
if git pull --ff-only >/tmp/git.out 2>/tmp/git.err; then exit 1; fi
rg -q '405' /tmp/git.err
if git push origin HEAD:refs/heads/e2e >/tmp/git.out 2>/tmp/git.err; then exit 1; fi
rg -q '405' /tmp/git.err
EOF
cp "$host_home/config/slopbox/config.toml" "$root/before-status-signing.toml"
cat >>"$host_home/config/slopbox/config.toml" <<EOF

[[git.identities]]
workspace = "$git_workspace"
name = "Test"
email = "test@example.com"
signing_key_fingerprint = "SHA256:unavailable"
EOF
env "${common_env[@]}" OPENROUTER_API_KEY= "$SLOPBOX" status --workspace "$git_workspace" >"$root/account-status.out"
assert_contains '^Signing +Test <test@example.com>.*key availability not checked$' "$root/account-status.out"
assert_contains '^Account route git: POST https://forge.slopbox-e2e.invalid/org/repo.git; available to agent and commands$' "$root/account-status.out"
assert_contains '^Git URL +ssh://git@forge.slopbox-e2e.invalid/org/repo.git -> authenticated route git$' "$root/account-status.out"
env "${common_env[@]}" OPENROUTER_API_KEY= SSH_AUTH_SOCK="$root/no-agent" \
  "$SLOPBOX" doctor --workspace "$git_workspace" >"$root/account-doctor.out"
assert_contains '^OK +Account configuration: 1 route' "$root/account-doctor.out"
assert_contains '^OK +Signing configuration: identity configured; signing key not queried' "$root/account-doctor.out"
assert_contains '^0 failed check' "$root/account-doctor.out"
cp "$root/before-status-signing.toml" "$host_home/config/slopbox/config.toml"
slopbox policy --workspace "$git_workspace" >"$root/git-policy.out"
assert_contains '^git-url: ssh://git@forge.slopbox-e2e.invalid/org/repo.git -> git$' "$root/git-policy.out"
slopbox policy --workspace "$workspace" >"$root/other-policy.out"
assert_not_contains 'git-url:' "$root/other-policy.out"
for mode in live staged; do
  cat >"$git_workspace/.slopbox.toml" <<EOF
[policy]
workspace = "$mode"
network = "none"
credentials = "none"
harness = "none"
EOF
  slopbox status --workspace "$git_workspace" >"$root/git-status.out"
  assert_contains '^Internet +General egress disabled; model and account routes are separate$' "$root/git-status.out"
  assert_contains '^Model +Disabled$' "$root/git-status.out"
  assert_contains '^Account route git:' "$root/git-status.out"
  if [[ "$mode" = staged ]]; then
    assert_contains '^Changes +Kept separate;' "$root/git-status.out"
  fi
  slopbox run --workspace "$git_workspace" --dev-env none -- sh -eu -c '
    sh check-git.sh
    slopbox tool-run --network none -- sh check-git.sh
  '
  cmp "$root/original-gitconfig" "$git_workspace/.git/config"
done
stage_id=$(slopbox stage list --workspace "$git_workspace" | awk '{ print $1 }')
slopbox stage discard --workspace "$git_workspace" "$stage_id" >/dev/null
assert_not_contains "$canary" "$git_workspace" "$host_home/data/slopbox/boxes"
cp "$host_home/config/slopbox/config.base.toml" "$host_home/config/slopbox/config.toml"

if [[ -n "$SOURCE_ROOT" && -S /nix/var/nix/daemon-socket/socket ]]; then
  echo "e2e: contained project closure"
  contained_workspace="$root/contained-workspace"
  mkdir "$contained_workspace"
  cp "$SOURCE_ROOT/flake.nix" "$SOURCE_ROOT/flake.lock" "$SOURCE_ROOT/Cargo.lock" "$contained_workspace/"
  cat >"$contained_workspace/.slopbox.toml" <<'EOF'
[policy]
network = "none"
credentials = "none"
harness = "none"
EOF
  realpath /run/current-system/sw/bin/nix-store >"$contained_workspace/unrelated-store-path"
  host_store_count=$(find /nix/store -mindepth 1 -maxdepth 1 | wc -l)
  slopbox run --workspace "$contained_workspace" --profile contained -- sh -eu -c '
    rustc --version >contained-result
    test "$PI_OFFLINE" = 1
    test ! -e /run/current-system/sw/bin/nix
    test ! -e "$(cat unrelated-store-path)"
    find /nix/store -mindepth 1 -maxdepth 1 | wc -l >contained-store-count
  ' 2>&1 | tee "$root/contained-run.err" >&2
  contained_stage=$(sed -n 's/^slopbox: staged workspace retained at //p' "$root/contained-run.err" | tail -n 1)
  test -n "$contained_stage"
  assert_contains '^rustc ' "$contained_stage/contained-result"
  guest_store_count=$(cat "$contained_stage/contained-store-count")
  if (( guest_store_count >= host_store_count )); then
    echo "contained runtime exposed the full host store" >&2
    exit 1
  fi
  contained_id=$(basename "$(dirname "$contained_stage")")
  slopbox stage discard --workspace "$contained_workspace" "$contained_id" >/dev/null
fi

assert_not_contains "$canary" "$workspace" "$host_home/data/slopbox"
assert_not_contains "$codex_canary" "$workspace" "$host_home/data/slopbox/boxes"
echo "e2e: all checks passed"
