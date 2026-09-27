"""Opt-in, paid Claude/OpenRouter acceptance. Only generated workspace data is sent."""

import argparse
import hashlib
import http.client
import json
import os
import secrets
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("slopbox", type=Path)
parser.add_argument("claude", type=Path)
parser.add_argument("node", type=Path)
parser.add_argument("--live", action="store_true", required=True)
args = parser.parse_args()
assert sys.platform == "darwin", "This live fixture currently targets native macOS"
key = os.environ.get("SLOPBOX_LIVE_OPENROUTER_KEY")
assert key, "Provide SLOPBOX_LIVE_OPENROUTER_KEY through a host-side secret source"
model = "anthropic/claude-haiku-4.5"
claude = args.claude.absolute()
node = args.node.resolve()
assert hashlib.sha256(claude.read_bytes()).hexdigest() == (
    "d8cb1e5c79684cc12a8bfc813e3a2073406921b6245744b3009be3ab5651d21e"
), "Expected the original Claude Code 2.1.283 darwin-arm64 artifact"


def credit_status():
    connection = http.client.HTTPSConnection("openrouter.ai", timeout=20)
    try:
        connection.request(
            "GET", "/api/v1/key", headers={"Authorization": "Bearer " + key}
        )
        response = connection.getresponse()
        assert response.status == 200, f"Key status returned HTTP {response.status}"
        data = json.loads(response.read())["data"]
        return {name: data.get(name) for name in ("limit", "limit_remaining", "usage")}
    finally:
        connection.close()


before = credit_status()
assert before["limit"] is not None and 0 < before["limit"] <= 1, (
    "Use a dedicated key with a provider-side credit limit of at most $1"
)
assert before["limit_remaining"] > 0, "The test key has no remaining credit"
print(json.dumps({"credit_before": before}), flush=True)
with tempfile.TemporaryDirectory(
    prefix="slopbox-claude-live-", dir="/private/var/tmp"
) as directory:
    root = Path(directory)
    home = root / "home"
    config = home / ".config/slopbox/config.toml"
    config.parent.mkdir(parents=True)
    (home / ".ssh").mkdir()
    secret = home / ".ssh/canary"
    secret.write_text("disposable-host-canary")
    binary = root / "slopbox"
    shutil.copyfile(args.slopbox.resolve(), binary)
    binary.chmod(0o755)
    config.write_text(f"""[policy]
network = "none"
credentials = "none"
harness = "none"
[runtime]
executables = {json.dumps([str(claude), str(node), "cat", "uname", "sleep", "chmod", "mkdir", "rm", "touch"])}
[defaults]
accounts = ["openrouter"]
[secrets.openrouter]
source = "environment"
variable = "HOST_OPENROUTER_KEY"
[[http_routes]]
name = "openrouter"
upstream = "https://openrouter.ai/api"
methods = ["POST"]
authentication = {{ type = "bearer", secret = "openrouter" }}
""")
    environment = {
        "HOME": str(home),
        "PATH": "/usr/bin:/bin:/usr/sbin:/sbin",
        "HOST_OPENROUTER_KEY": key,
    }
    for relative in ("first", "unrelated/second"):
        assert credit_status()["limit_remaining"] > 0.2, (
            "Insufficient credit for another run"
        )
        workspace = root / relative
        workspace.mkdir(parents=True)
        marker = secrets.token_hex(12)
        (workspace / "answer.txt").write_text(marker + "\n")
        probe = workspace / "probe.mjs"
        probe.write_text(f"""
import assert from 'node:assert/strict';
import * as fs from 'node:fs';
import * as net from 'node:net';
assert(!process.env.HOST_OPENROUTER_KEY);
assert(!process.env.SLOPBOX_LIVE_OPENROUTER_KEY);
assert(!process.env.OPENROUTER_API_KEY);
assert(!process.env.SSH_AUTH_SOCK);
assert(!process.env.SLOPBOX_MODEL_PROXY_PORT);
assert.equal(process.env.ANTHROPIC_AUTH_TOKEN, 'slopbox:openrouter');
for (const file of {json.dumps([str(config), str(secret)])}) {{
  assert.throws(() => fs.readFileSync(file), {{code:'EPERM'}});
}}
assert.throws(() => fs.openSync({json.dumps(str(claude))}, 'r+'), {{code:'EPERM'}});
await assert.rejects(new Promise((resolve, reject) => {{
  const socket = net.createConnection({{host:'1.1.1.1', port:443}});
  socket.once('error', reject);
  socket.once('connect', () => {{ socket.destroy(); resolve(); }});
  socket.setTimeout(2000, () => socket.destroy(new Error('connect timeout')));
}}), {{code:'EPERM'}});
assert.equal(fs.readFileSync('answer.txt', 'utf8'), 'live-haiku-passed\\n');
fs.writeFileSync('probe-passed', 'passed');
console.log('probe-complete');
""")
        original_probe = probe.read_bytes()
        command = [
            str(binary),
            "run",
            "--dev-env",
            "none",
            "--",
            "bash",
            "-c",
            'export ANTHROPIC_BASE_URL="$SLOPBOX_AUTHENTICATED_HTTP_BASE_URL/openrouter" CLAUDE_CODE_TMPDIR="$TMPDIR"; exec "$@"',
            "live-fixture",
            "env",
            "ANTHROPIC_AUTH_TOKEN=slopbox:openrouter",
            "ANTHROPIC_API_KEY=",
            f"ANTHROPIC_DEFAULT_HAIKU_MODEL={model}",
            f"ANTHROPIC_DEFAULT_SONNET_MODEL={model}",
            f"ANTHROPIC_DEFAULT_OPUS_MODEL={model}",
            "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1",
            "CLAUDE_CODE_DISABLE_OFFICIAL_MARKETPLACE_AUTOINSTALL=1",
            "CLAUDE_CODE_MAX_OUTPUT_TOKENS=1024",
            "MAX_THINKING_TOKENS=0",
            str(claude),
            "-p",
            (
                "Use Read on answer.txt. Use Edit to replace its one-line contents with live-haiku-passed, "
                "preserving the final newline. Then use Bash to run exactly: node probe.mjs. "
                "Do not modify probe.mjs or run any other commands. Report the probe output briefly."
            ),
            "--model",
            model,
            "--tools",
            "Read,Edit,Bash",
            "--allowedTools",
            "Read,Edit,Bash",
            "--permission-mode",
            "dontAsk",
            "--max-turns",
            "6",
            "--max-budget-usd",
            "0.20",
            "--no-session-persistence",
            "--output-format",
            "stream-json",
            "--verbose",
            "--include-partial-messages",
        ]
        result = subprocess.run(
            command,
            check=False,
            env=environment,
            cwd=workspace,
            stdin=subprocess.DEVNULL,
            capture_output=True,
            text=True,
            timeout=180,
        )
        assert key not in result.stdout + result.stderr, (
            "Credential appeared in captured output"
        )
        assert result.returncode == 0, result.stderr + result.stdout
        events = [
            json.loads(line)
            for line in result.stdout.splitlines()
            if line.startswith("{")
        ]
        final = [event for event in events if event.get("type") == "result"][-1]
        assert not final.get("is_error"), final
        assert not final.get("permission_denials"), final
        assert any(event.get("type") == "stream_event" for event in events), (
            "No partial events"
        )
        calls = [
            block
            for event in events
            if event.get("type") == "assistant"
            for block in event["message"].get("content", [])
            if block.get("type") == "tool_use"
        ]
        assert {call["name"] for call in calls} == {"Read", "Edit", "Bash"}, calls
        assert any(
            call["name"] == "Bash" and call["input"]["command"] == "node probe.mjs"
            for call in calls
        )
        assert marker in result.stdout, "Read result missing"
        assert "probe-complete" in result.stdout
        assert (workspace / "answer.txt").read_text() == "live-haiku-passed\n"
        assert (workspace / "probe-passed").read_text() == "passed"
        assert probe.read_bytes() == original_probe, "Probe was modified"
        usage = final.get("modelUsage", {})
        assert usage and all("haiku" in name.lower() for name in usage), usage
        print(
            json.dumps(
                {
                    "workspace": relative,
                    "result": "passed",
                    "models": list(usage),
                    "reported_cost_usd": final.get("total_cost_usd"),
                    "turns": final.get("num_turns"),
                }
            ),
            flush=True,
        )
print(json.dumps({"credit_after": credit_status()}), flush=True)
