"""Opt-in unmodified Pi acceptance against the capped OpenRouter test account."""

import json
import os
import pwd
import selectors
import subprocess
import sys
import tempfile
import time
import urllib.request
from pathlib import Path

assert sys.platform == "darwin" and len(sys.argv) == 7 and sys.argv[-1] == "--live"
slopbox, pi, node, bundle, dependencies = (
    Path(value).absolute() for value in sys.argv[1:6]
)
account = pwd.getpwuid(os.getuid()).pw_name
key = subprocess.run(
    [
        "/usr/bin/security",
        "find-generic-password",
        "-a",
        account,
        "-s",
        "slopbox-openrouter-test",
        "-w",
    ],
    check=True,
    capture_output=True,
    text=True,
    timeout=20,
).stdout.strip()
request = urllib.request.Request(
    "https://openrouter.ai/api/v1/key", headers={"Authorization": f"Bearer {key}"}
)
with urllib.request.urlopen(request, timeout=20) as response:
    budget = json.load(response)["data"]
assert 0 < budget["limit"] <= 1 and budget["limit_remaining"] > 0.20
print(
    json.dumps(
        {field: budget[field] for field in ("limit", "limit_remaining", "usage")}
    )
)
model = "anthropic/claude-haiku-4.5"
with tempfile.TemporaryDirectory(
    prefix="slopbox-pi-live-", dir="/private/var/tmp"
) as temporary:
    root = Path(temporary)
    work = root / "work"
    work.mkdir()
    (root / "canary").write_text("unrelated host data")
    (work / "task.txt").write_text("OLD\n")
    probe = (
        "const fs=require('node:fs'), assert=require('node:assert/strict'), net=require('node:net');\n"
        f"assert.throws(()=>fs.readFileSync({json.dumps(str(root / 'canary'))}),{{code:'EPERM'}});\n"
        f"assert.throws(()=>fs.openSync({json.dumps(str(pi))},'r+'),{{code:'EPERM'}});\n"
        "for(const key of ['HOST_OPENROUTER_KEY','SLOPBOX_LIVE_OPENROUTER_KEY','SSH_AUTH_SOCK','SLOPBOX_MODEL_PROXY_PORT']) assert.equal(process.env[key],undefined);\n"
        "assert.equal(process.env.OPENROUTER_API_KEY,'slopbox:openrouter');\n"
        "const s=net.connect({host:'1.1.1.1',port:443});\n"
        "s.once('connect',()=>{throw new Error('direct network escaped')});\n"
        "s.once('error',e=>{assert.equal(e.code,'EPERM'); fs.writeFileSync('probe-result','isolation passed'); console.log('isolation passed')});\n"
        "s.setTimeout(3000,()=>{throw new Error('network denial timed out')});\n"
    )
    (work / "probe.cjs").write_text(probe)
    config = root / "config/slopbox/config.toml"
    config.parent.mkdir(parents=True)
    config.write_text(
        '[policy]\nnetwork="allowlist"\ncredentials="none"\nharness="none"\n'
        f"[runtime]\nexecutables={json.dumps([str(pi), str(node)])}\n"
        f"bundles={json.dumps([str(bundle)])}\ndependency_roots={json.dumps([str(dependencies)])}\n"
        '[defaults]\naccounts=["openrouter"]\n'
        '[secrets.openrouter]\nsource="environment"\nvariable="HOST_OPENROUTER_KEY"\n'
        '[[http_routes]]\nname="openrouter"\nupstream="https://openrouter.ai/api"\nmethods=["POST"]\nproxy=true\n'
        'authentication={type="bearer",secret="openrouter"}\n'
        '[environment]\nOPENROUTER_API_KEY="slopbox:openrouter"\nPI_OFFLINE="1"\n'
        'NODE_USE_ENV_PROXY="1"\nNODE_EXTRA_CA_CERTS="${SLOPBOX_ACCOUNT_CA}"\nOPENSSL_CONF="/dev/null"\n'
    )
    config.chmod(0o600)
    environment = dict(
        os.environ,
        XDG_CONFIG_HOME=str(root / "config"),
        XDG_DATA_HOME=str(root / "data"),
        HOST_OPENROUTER_KEY=key,
    )
    prefix = [str(slopbox), "run", "--workspace", str(work), "--"]
    models = {
        "providers": {
            "openrouter": {
                "modelOverrides": {
                    model: {"maxTokens": 512, "samplingParams": {"max_tokens": 512}}
                }
            }
        }
    }
    seed = (
        "const fs=require('node:fs'); const p=process.env.HOME+'/.pi/agent';"
        f"fs.mkdirSync(p,{{recursive:true}});fs.writeFileSync(p+'/models.json',{json.dumps(json.dumps(models))});"
    )
    seeded = subprocess.run(
        prefix + ["node", "-e", seed],
        env=environment,
        check=False,
        capture_output=True,
        text=True,
        timeout=40,
    )
    assert seeded.returncode == 0, seeded.stderr.replace(key, "[REDACTED]")
    command = prefix + [
        "pi",
        "--provider",
        "openrouter",
        "--model",
        model,
        "--thinking",
        "off",
        "--mode",
        "json",
        "--no-session",
        "--no-extensions",
        "--no-skills",
        "--no-prompt-templates",
        "--no-themes",
        "--tools",
        "read,edit,bash",
        "-p",
        "Use read on task.txt, use edit to replace OLD with NEW, then use bash to run exactly `node probe.cjs`. Do not modify probe.cjs or run any other command. Finally reply READY.",
    ]
    process = subprocess.Popen(
        command,
        env=environment,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
    )
    events, output, pending = [], bytearray(), bytearray()
    turns = 0
    deadline = time.monotonic() + 120
    try:
        with selectors.DefaultSelector() as selector:
            selector.register(process.stdout, selectors.EVENT_READ)
            while selector.get_map():
                assert time.monotonic() < deadline, "Pi inference timed out"
                for selected, _ in selector.select(0.1):
                    chunk = os.read(selected.fd, 65536)
                    if not chunk:
                        selector.unregister(selected.fileobj)
                        continue
                    output.extend(chunk)
                    pending.extend(chunk)
                    assert len(output) <= 4 * 1024 * 1024, "Pi output limit exceeded"
                    while b"\n" in pending:
                        line, _, pending = pending.partition(b"\n")
                        try:
                            event = json.loads(line)
                        except json.JSONDecodeError:
                            continue
                        events.append(event)
                        if (
                            event.get("type") == "message_start"
                            and event.get("message", {}).get("role") == "assistant"
                        ):
                            turns += 1
                            assert turns <= 6, "Pi turn limit exceeded"
        code = process.wait(timeout=10)
    finally:
        if process.poll() is None:
            process.terminate()
            try:
                process.wait(timeout=15)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=5)
        process.stdout.close()
    text = output.decode(errors="replace")
    assert key not in text, "host credential appeared in guest output"
    assert code == 0, text[-8000:]
    messages = [
        event["message"]
        for event in events
        if event.get("type") == "message_end"
        and event.get("message", {}).get("role") == "assistant"
    ]
    assert messages and all(
        message["model"] == model and message["provider"] == "openrouter"
        for message in messages
    ), text[-8000:]
    tools = [
        event["toolName"]
        for event in events
        if event.get("type") == "tool_execution_end"
    ]
    assert {"read", "edit", "bash"} <= set(tools), text[-8000:]
    assert any(event.get("type") == "message_update" for event in events), (
        "stream events missing"
    )
    assert (work / "task.txt").read_text() == "NEW\n"
    assert (work / "probe.cjs").read_text() == probe
    assert (work / "probe-result").read_text() == "isolation passed"
    assert any(
        block.get("text", "").strip() == "READY" for block in messages[-1]["content"]
    )
    print(json.dumps({"turns": turns, "tools": tools, "model": model}))
    print("unmodified Pi: streamed inference, read/edit/bash and isolation passed")
