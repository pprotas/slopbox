"""Opt-in paid Claude/OpenRouter terminal and cross-run resume acceptance."""

import argparse
import fcntl
import hashlib
import http.client
import json
import os
import pty
import re
import select
import signal
import struct
import sys
import tempfile
import termios
import time
import uuid
from pathlib import Path

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("slopbox", type=Path)
parser.add_argument("claude", type=Path)
parser.add_argument("--live", action="store_true", required=True)
args = parser.parse_args()
assert sys.platform == "darwin"
key = os.environ.get("SLOPBOX_LIVE_OPENROUTER_KEY")
assert key, "Supply SLOPBOX_LIVE_OPENROUTER_KEY through a host-side secret source"
binary = str(args.slopbox.resolve())
claude = args.claude.absolute()
assert hashlib.sha256(claude.read_bytes()).hexdigest() == (
    "d8cb1e5c79684cc12a8bfc813e3a2073406921b6245744b3009be3ab5651d21e"
), "Expected the original Claude Code 2.1.283 darwin-arm64 artifact"
model = "anthropic/claude-haiku-4.5"


def credit_status():
    connection = http.client.HTTPSConnection("openrouter.ai", timeout=20)
    try:
        connection.request(
            "GET", "/api/v1/key", headers={"Authorization": "Bearer " + key}
        )
        response = connection.getresponse()
        assert response.status == 200, f"Key status returned HTTP {response.status}"
        data = json.loads(response.read())["data"]
        values = {
            name: data.get(name) for name in ("limit", "limit_remaining", "usage")
        }
        assert all(type(value) in (int, float) for value in values.values()), (
            "Expected a limited key"
        )
        assert 0 < values["limit"] <= 1, "Use a dedicated key capped at $1 or less"
        return values
    finally:
        connection.close()


print(json.dumps({"credit_before": credit_status()}), flush=True)
with tempfile.TemporaryDirectory(
    prefix="slopbox-claude-pty-", dir="/private/var/tmp"
) as directory:
    root = Path(directory)
    home = root / "host-home"
    config = home / ".config/slopbox/config.toml"
    config.parent.mkdir(parents=True)
    workspace = root / "workspace"
    workspace.mkdir()
    token = "remember-" + os.urandom(8).hex()
    (workspace / "challenge.txt").write_text(token + "\n")
    config.write_text(f"""[policy]
network = "none"
credentials = "none"
harness = "none"
[runtime]
executables = {json.dumps([str(claude), "cat", "uname", "sleep", "chmod", "mkdir", "rm", "touch"])}
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
        "TERM": "xterm-256color",
    }
    session = str(uuid.uuid4())
    seen = set()
    for index in range(2):
        assert credit_status()["limit_remaining"] > 0.2, "Insufficient test credit"
        command = [
            binary,
            "run",
            "--dev-env",
            "none",
            "--",
            "bash",
            "-c",
            'export ANTHROPIC_BASE_URL="$SLOPBOX_AUTHENTICATED_HTTP_BASE_URL/openrouter" CLAUDE_CODE_TMPDIR="$TMPDIR"; printf %s "$HOME" > guest-home-path; exec "$@"',
            "interactive",
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
            "--model",
            model,
            "--permission-mode",
            "dontAsk",
            "--tools",
            "Read" if index == 0 else "",
            "--allowedTools",
            "Read",
            "--session-id" if index == 0 else "--resume",
            session,
        ]
        if index == 0:
            command.append("Read challenge.txt and reply only with its contents.")
        master, slave = pty.openpty()
        os.set_blocking(master, False)
        saved = termios.tcgetattr(slave)
        fcntl.ioctl(master, termios.TIOCSWINSZ, struct.pack("HHHH", 35, 140, 0, 0))
        pid = os.fork()
        if pid == 0:
            os.setsid()
            fcntl.ioctl(slave, termios.TIOCSCTTY, 0)
            for descriptor in (0, 1, 2):
                os.dup2(slave, descriptor)
            os.close(master)
            os.close(slave)
            os.chdir(workspace)
            os.execve(binary, command, environment)
        output = bytearray()
        deadline = time.monotonic() + 90
        theme = trusted = notes = prompted = submitted = quitting = exited = False
        try:
            while True:
                assert time.monotonic() < deadline, "Terminal deadline exceeded"
                if select.select([master], [], [], 0.05)[0]:
                    try:
                        data = os.read(master, 65536)
                    except BlockingIOError:
                        data = b""
                    output.extend(data)
                    assert len(output) < 4 * 1024 * 1024
                    assert key.encode() not in output, "Credential in terminal output"
                    if b"\x1b[6n" in data:
                        os.write(master, b"\x1b[1;1R")
                    if b"\x1b[c" in data:
                        os.write(master, b"\x1b[?1;2c")
                    text = re.sub(
                        r"\x1b\[[0-?]*[ -/]*[@-~]", "", output.decode(errors="replace")
                    )
                    compact = "".join(text.split())
                    if not theme and "Choosethetextstyle" in compact:
                        os.write(master, b"\r")
                        theme = True
                    if (
                        not notes
                        and "Securitynotes:" in compact
                        and "PressEntertocontinue" in compact
                    ):
                        os.write(master, b"\r")
                        notes = True
                    if not trusted and "Yes,Itrustthisfolder" in compact:
                        time.sleep(0.2)
                        os.write(master, b"\x1b[B\r")
                        trusted = True
                    if index == 1 and not prompted and "don'taskon" in compact:
                        time.sleep(0.2)
                        os.write(
                            master,
                            b"Without using tools, recall the exact contents of the file you read earlier. Reply only with those contents.",
                        )
                        prompted = True
                    if (
                        prompted
                        and not submitted
                        and "Replyonlywiththosecontents." in compact
                    ):
                        os.write(master, b"\r")
                        submitted = True
                marker = workspace / "guest-home-path"
                if marker.exists() and (home_path := marker.read_text()):
                    guest_home = Path(home_path).resolve()
                    assert (
                        guest_home.parent.parent == home / ".local/share/slopbox/boxes"
                    )
                    assert guest_home.name == "native-home"
                    messages = []
                    for transcript in guest_home.glob(
                        f".claude/projects/*/{session}.jsonl"
                    ):
                        assert transcript.resolve().is_relative_to(guest_home)
                        contents = transcript.read_text()
                        assert key not in contents, "Credential in transcript"
                        for line in contents.splitlines(keepends=True):
                            if not line.endswith("\n"):
                                continue
                            event = json.loads(line)
                            if event.get("type") == "assistant":
                                messages.append(event["message"])
                    replies = [
                        message
                        for message in messages
                        if message.get("id") not in seen
                        and token
                        in "".join(
                            block.get("text", "") for block in message["content"]
                        )
                    ]
                    if not quitting and replies:
                        assert all(
                            "haiku" in message["model"].lower() for message in replies
                        )
                        assert index == 0 or submitted
                        calls = [
                            block
                            for message in messages
                            if message.get("id") not in seen
                            for block in message["content"]
                            if block.get("type") == "tool_use"
                        ]
                        if index == 0:
                            assert any(call["name"] == "Read" for call in calls)
                        else:
                            assert not calls, "Resumed recall unexpectedly used tools"
                        time.sleep(1)
                        os.write(master, b"/exit\r")
                        quitting = True
                        seen.update(message.get("id") for message in messages)
                finished, status = os.waitpid(pid, os.WNOHANG)
                if finished:
                    exited = True
                    assert os.WIFEXITED(status) and os.WEXITSTATUS(status) == 0, status
                    assert quitting, (
                        "No new provider response containing the recalled token"
                    )
                    assert termios.tcgetattr(master) == saved, (
                        "Terminal settings not restored"
                    )
                    break
        except BaseException:
            if key.encode() not in output:
                print(output.decode(errors="replace")[-4000:], file=sys.stderr)
            raise
        finally:
            os.close(master)
            os.close(slave)
            if not exited:
                try:
                    os.kill(pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                os.waitpid(pid, 0)
        print(
            json.dumps(
                {"stage": "initial" if index == 0 else "resumed", "result": "passed"}
            ),
            flush=True,
        )
        (workspace / "challenge.txt").unlink(missing_ok=True)
print(json.dumps({"credit_after": credit_status()}), flush=True)
