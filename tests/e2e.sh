#!/usr/bin/env bash
set -euo pipefail

SLOPBOX=${1:?usage: tests/e2e.sh /path/to/slopbox [source] [tests]}
SLOPBOX=$(realpath "$SLOPBOX")
SOURCE_ROOT=${2:-}
TESTS_DIR=${3:-"$(dirname "$(realpath "${BASH_SOURCE[0]}")")"}
for command in bash bwrap curl find findmnt git node openssl rg sh ssh-add ssh-agent ssh-keygen; do
  command -v "$command" >/dev/null || { echo "missing test dependency: $command" >&2; exit 2; }
done

root=$(mktemp -d)
trap 'rm -rf "$root"' EXIT
home="$root/home"
workspace="$root/workspace"
mkdir -p "$home/config/slopbox" "$home/data" "$root/runtime" "$workspace"
chmod 700 "$home" "$root/runtime"
printf 'fixture\n' >"$workspace/input.txt"
printf 'private\n' >"$home/.ssh-canary"
cat >"$home/config/slopbox/config.toml" <<'EOF'
default_command = ["sh"]
[policy]
network = "none"
credentials = "none"
harness = "none"
EOF
export HOME="$home" XDG_CONFIG_HOME="$home/config" XDG_DATA_HOME="$home/data" XDG_RUNTIME_DIR="$root/runtime"

echo "e2e: generic default and explicit commands without Pi"
"$SLOPBOX" status --workspace "$workspace" >"$root/status"
rg -q '^Execution +Generic commands;' "$root/status"
"$SLOPBOX" doctor --workspace "$workspace" >"$root/doctor"
rg -q '^0 failed check' "$root/doctor"
(cd "$workspace" && "$SLOPBOX" -- -eu -c '
  test "$SLOPBOX_SANDBOX" = 1
  test ! -e "$HOME/.ssh-canary"
  test ! -e /run/slopbox-host/model/gateway.sock
  test -z "${OPENROUTER_API_KEY-}${SLOPBOX_MODEL_PROXY_PORT-}"
  cat input.txt >/dev/null
  slopbox tool-run --network none -- sh -c "test ! -e /run/slopbox-host/model/gateway.sock"
')
"$SLOPBOX" run --workspace "$workspace" --dev-env none -- sh -c 'test "$SLOPBOX_SANDBOX" = 1'

wait_for_file() {
  for _ in {1..200}; do
    [[ -e "$1" ]] && return
    sleep 0.05
  done
  echo "timed out waiting for $1" >&2
  return 1
}

# Project and session approvals affect the broker, not the guest's policy files.
cat >"$home/config/slopbox/config.toml" <<'EOF'
default_command = ["sh"]
[policy]
network = "allowlist"
credentials = "none"
harness = "none"
EOF
echo "e2e: deny, approve, revoke and reject guest policy changes"
"$SLOPBOX" run --workspace "$workspace" --dev-env none -- sh -eu -c '
  curl -sS --connect-timeout 2 https://packages.slopbox-e2e.invalid/ >/dev/null 2>&1 || true
  slopbox denials >.denials
  : >.ready
  count=0
  until test -e .approved; do count=$((count + 1)); test "$count" -lt 200; sleep 0.05; done
  curl -sS --connect-timeout 2 https://packages.slopbox-e2e.invalid/ >/dev/null 2>&1 || true
  slopbox denials >.after-approval
' >"$root/network.out" 2>"$root/network.err" &
active_pid=$!
wait_for_file "$workspace/.ready"
rg -q 'no matching allow rule' "$workspace/.denials"
"$SLOPBOX" network events --workspace "$workspace" >"$root/events"
request=$(awk '$2 == "CONNECT" && $3 == "packages.slopbox-e2e.invalid:443" {print $1; exit}' "$root/events")
test -n "$request"
"$SLOPBOX" network approve "$request" --project --workspace "$workspace" >"$root/approved"
rule=$(awk '{print $2}' "$root/approved")
test -n "$rule"
printf '%s\n' "$rule" >"$workspace/.rule"
: >"$workspace/.approved"
wait "$active_pid"
rg -q 'DNS resolution failed' "$workspace/.after-approval"
"$SLOPBOX" run --workspace "$workspace" --dev-env none -- sh -eu -c '
  rule=$(cat .rule)
  if slopbox network revoke "$rule" >.guest-revoke 2>&1; then exit 1; fi
  if slopbox network approve "'"$request"'" --project >.guest-approve 2>&1; then exit 1; fi
  curl -sS --connect-timeout 2 https://packages.slopbox-e2e.invalid/ >/dev/null 2>&1 || true
  slopbox denials >.project-denials
'
rg -q 'DNS resolution failed' "$workspace/.project-denials"
"$SLOPBOX" network revoke "$rule" --workspace "$workspace" >/dev/null
"$SLOPBOX" run --workspace "$workspace" --dev-env none -- sh -eu -c '
  curl -sS --connect-timeout 2 https://packages.slopbox-e2e.invalid/ >/dev/null 2>&1 || true
  slopbox denials >.revoked-denials
'
rg -q 'no matching allow rule' "$workspace/.revoked-denials"
"$SLOPBOX" network approvals --workspace "$workspace" --json >"$root/rules.json"
rg -q '"revoked_at_ms":[0-9]+' "$root/rules.json"
rm -f "$workspace"/.*-denials "$workspace"/.denials "$workspace"/.ready "$workspace"/.approved "$workspace"/.rule "$workspace"/.guest-revoke "$workspace"/.guest-approve

if [[ -n "$SOURCE_ROOT" ]]; then
  command -v nix >/dev/null || { echo "missing test dependency: nix" >&2; exit 2; }
  echo "e2e: contained project closure"
  contained="$root/contained"
  mkdir "$contained"
  cp "$SOURCE_ROOT/flake.nix" "$SOURCE_ROOT/flake.lock" "$SOURCE_ROOT/Cargo.lock" "$contained/"
  cat >"$contained/.slopbox.toml" <<'EOF'
[policy]
network = "none"
credentials = "none"
harness = "none"
EOF
  realpath "$(type -P nix)" >"$contained/unrelated-store-path"
  host_store_count=$(find /nix/store -mindepth 1 -maxdepth 1 | wc -l)
  "$SLOPBOX" run --workspace "$contained" --profile contained -- sh -eu -c '
    rustc --version >contained-result
    test ! -e /run/current-system/sw/bin/nix
    test ! -e "$(cat unrelated-store-path)"
    find /nix/store -mindepth 1 -maxdepth 1 | wc -l >contained-store-count
  ' 2>&1 | tee "$root/contained-run.err" >&2
  stage=$(sed -n 's/^slopbox: staged workspace retained at //p' "$root/contained-run.err" | tail -n 1)
  test -n "$stage"
  rg -q '^rustc ' "$stage/contained-result"
  guest_store_count=$(cat "$stage/contained-store-count")
  (( guest_store_count < host_store_count ))
  stage_id=$(basename "$(dirname "$stage")")
  "$SLOPBOX" stage discard --workspace "$contained" "$stage_id" >/dev/null
fi

echo "e2e: shared identity/accounts and mediated HTTPS clients"
SHELL=$(type -P bash) node "$TESTS_DIR/linux-accounts.mjs" "$SLOPBOX"
echo "e2e: all checks passed"
