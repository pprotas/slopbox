import fcntl
import json
import os
from pathlib import Path
import pty
import re
import select
import signal
import struct
import subprocess
import sys
import tempfile
import termios
import time

slopbox, node = sys.argv[1:]
header = rb"SLOPBOX HOST APPROVALS"
prompt = rb"\r\n> "
challenge = rb"Type ([a-f0-9]{12}) and Enter to confirm"

for scenario in ["approvals", "plain", "disabled", "exit-in-view"]:
    with tempfile.TemporaryDirectory(prefix="slopbox-approvals-", dir="/private/var/tmp") as directory:
        root = Path(directory)
        workspace = root / "workspace"
        for name in ["workspace", "home", "config/slopbox", "data"]:
            (root / name).mkdir(parents=True)
        (workspace / "approvals-probe.mjs").write_bytes(Path(__file__).with_name("approvals-probe.mjs").read_bytes())
        network = "none" if scenario == "disabled" else "allowlist"
        (root / "config/slopbox/config.toml").write_text(
            f'[policy]\nharness="none"\ncredentials="none"\nnetwork="{network}"\n'
            f'[runtime]\nexecutables={json.dumps([node])}\n'
        )
        environment = {
            "HOME": str(root / "home"), "XDG_CONFIG_HOME": str(root / "config"),
            "XDG_DATA_HOME": str(root / "data"), "PATH": "/usr/bin:/bin", "TERM": "xterm-256color",
        }
        status = subprocess.run([slopbox, "status", "--verbose"], cwd=workspace, env=environment,
                                capture_output=True, text=True, check=True)
        state = Path(re.search(r"^project-state: (.+)$", status.stdout, re.M)[1])
        rules_path = state / "network-rules.json"
        master, slave = pty.openpty()
        os.set_blocking(master, False)
        saved = termios.tcgetattr(slave)
        fcntl.ioctl(master, termios.TIOCSWINSZ, struct.pack("HHHH", 32, 120, 0, 0))
        (workspace / "probe.json").write_text(json.dumps({"tty": os.ttyname(slave), "rules": str(rules_path), "network": network}))
        pid = os.fork()
        if pid == 0:
            os.setsid()
            fcntl.ioctl(slave, termios.TIOCSCTTY, 0)
            for target in [0, 1, 2]:
                os.dup2(slave, target)
            os.close(master)
            os.close(slave)
            os.chdir(workspace)
            arguments = [slopbox, "run", "--dev-env", "none"]
            if scenario != "plain":
                arguments.append("--approval-view")
            os.execve(slopbox, arguments + ["--", node, "approvals-probe.mjs"], environment)
        output = bytearray()
        pending = bytearray()
        deadline = time.monotonic() + 45
        exited = False

        def pump():
            assert time.monotonic() < deadline, f"{scenario}: terminal timeout: {bytes(output[-6000:])!r}"
            if select.select([master], [], [], .02)[0]:
                try:
                    chunk = os.read(master, 65536)
                except BlockingIOError:
                    return
                output.extend(chunk)
                pending.extend(chunk)
                assert len(output) < 2 * 1024 * 1024

        def wait_for(pattern):
            while True:
                match = re.search(pattern, pending)
                if match:
                    found = match.groups()
                    del pending[:match.end()]
                    return found
                pump()

        def submit(text, pattern):
            os.write(master, text + b"\n")
            found = wait_for(pattern)
            wait_for(prompt)
            return found

        def rules():
            return json.loads(rules_path.read_text())["rules"] if rules_path.exists() else []

        def guest_input():
            path = workspace / "guest-input"
            return path.read_bytes() if path.exists() else b""

        try:
            wait_for(rb"PROBE_READY")
            assert header not in output, "guest output opened the host view"
            if scenario == "plain":
                os.write(master, b"\x1d")
                wait_for(rb"LITERAL_CTRL_BRACKET")
                assert header not in output
                assert b"Ctrl-] opens" not in output
                assert guest_input() == b"\x1d"
            else:
                os.write(master, b"\x1dp 1\nyes\n")
                wait_for(header)
                wait_for(prompt)
                assert rules() == [], "queued input changed approvals"
                assert guest_input() == b"", "host approval input reached the guest"
                if scenario == "approvals":
                    response = (workspace / "response-1").read_text()
                    assert "no matching allow rule" in response, f"initial response: {response!r}"
                    (workspace / "host-diagnostic").touch()
                    while not (workspace / "diagnostic-done").exists():
                        pump()
                    assert b"GUEST_WHILE_HOST_VIEW" not in output
                    assert b"slopbox general gateway:" not in output
                    cancelled = submit(b"s 1", challenge)[0]
                    assert b"Approve view.slopbox-native.invalid:80" in output
                    assert b"Scope: this session only" in output
                    submit(b"yes", rb"confirmation cancelled")
                    assert rules() == []
                    small = submit(b"s 1", challenge)[0]
                    assert small != cancelled
                    fcntl.ioctl(master, termios.TIOCSWINSZ, struct.pack("HHHH", 20, 60, 0, 0))
                    wait_for(rb"Resize to at least 80x24")
                    wait_for(prompt)
                    submit(small, rb"Resize to at least 80x24")
                    assert rules() == [], "small terminal accepted a confirmation"
                    fcntl.ioctl(master, termios.TIOCSWINSZ, struct.pack("HHHH", 32, 120, 0, 0))
                    wait_for(header)
                    wait_for(prompt)
                    code = submit(b"s 1", challenge)[0]
                    assert code not in [small, cancelled]
                    submit(code, rb"Approved rule-[a-f0-9]+; retry the operation")
                    assert len(rules()) == 1 and rules()[0]["scope"] == "session"
                    assert (workspace / "attempts").read_text() == "1"
                    assert sum(len(path.read_text().splitlines()) for path in state.glob("sessions/*/events.jsonl")) == 1, "approval replayed the request"
                    assert guest_input() == b""
                    os.write(master, b"q\n")
                    wait_for(rb"GUEST_WHILE_HOST_VIEW")
                    wait_for(rb"SIZE:120,32")
                    os.write(master, b"guards\n")
                    wait_for(rb"GUARDS_DENIED")
                    os.write(master, b"retry\n")
                    wait_for(rb"RETRIED")
                    response = (workspace / "response-2").read_text()
                    assert "DNS resolution failed" in response, f"approved response: {response!r}"
                    # Exercise fragmented enhanced keyboard input as well as the control byte.
                    for part in [b"\x1b[93;", b"5u"]:
                        os.write(master, part)
                        pump()
                    wait_for(header)
                    index = wait_for(rb"(\d+): RULE view\.slopbox-native\.invalid:80 session")[0]
                    wait_for(prompt)
                    code = submit(b"r " + index, challenge)[0]
                    submit(code, rb"Revoked rule-")
                    assert rules()[0]["revoked_at_ms"] is not None
                    code = submit(b"p 1", challenge)[0]
                    assert b"Scope: PROJECT: persists across sessions" in output
                    submit(code, rb"Approved rule-")
                    assert len(rules()) == 2 and rules()[1]["scope"] == "project"
                    index = re.findall(rb"(\d+): RULE view\.slopbox-native\.invalid:80 project", output)[-1]
                    code = submit(b"r " + index, challenge)[0]
                    submit(code, rb"Revoked rule-")
                    assert all(rule["revoked_at_ms"] is not None for rule in rules())
                    # Suspension while the host view is open must restore termios too.
                    os.kill(pid, signal.SIGTSTP)
                    while True:
                        stopped, child_status = os.waitpid(pid, os.WNOHANG | os.WUNTRACED)
                        if stopped:
                            assert os.WIFSTOPPED(child_status)
                            break
                        pump()
                    assert termios.tcgetattr(slave) == saved
                    os.kill(pid, signal.SIGCONT)
                    wait_for(header)
                    wait_for(prompt)
                    os.write(master, b"]\n")
                    wait_for(rb"LITERAL_CTRL_BRACKET")
                    wait_for(rb"SIZE:120,32")
                    os.write(master, b"retry\n")
                    wait_for(rb"RETRIED")
                    response = (workspace / "response-3").read_text()
                    assert "no matching allow rule" in response, f"revoked response: {response!r}"
                    assert guest_input() == b"guards\nretry\n\x1dretry\n"
                    # A corrupt host rule file must produce a safe view, not a grant.
                    original = rules_path.read_bytes()
                    rules_path.write_text("not json")
                    os.write(master, b"\x1d")
                    wait_for(rb"Cannot inspect network state")
                    wait_for(prompt)
                    submit(b"s 1", rb"entry number is out of range")
                    os.write(master, b"q\n")
                    wait_for(rb"SIZE:120,32")
                    assert rules_path.read_text() == "not json"
                    rules_path.write_bytes(original)
                elif scenario == "disabled":
                    assert b"General networking is disabled" in output
                    submit(b"s 1", rb"entry number is out of range")
                    assert rules() == []
                    os.write(master, b"q\n")
                    wait_for(rb"SIZE:120,32")
                else:
                    (workspace / "host-fail").touch()
            if scenario != "exit-in-view":
                os.write(master, b"exit\n")
            while True:
                pump()
                finished, child_status = os.waitpid(pid, os.WNOHANG)
                if finished:
                    exited = True
                    expected = 7 if scenario == "exit-in-view" else 0
                    assert os.WIFEXITED(child_status) and os.WEXITSTATUS(child_status) == expected, (child_status, bytes(output[-6000:]))
                    break
            assert termios.tcgetattr(master) == saved, "host terminal settings were not restored"
            if scenario == "approvals":
                assert b"recent broker diagnostics" in output
                assert b"slopbox general gateway:" in output
            print(f"native approval view {scenario} passed")
        finally:
            os.close(master)
            os.close(slave)
            if not exited:
                os.kill(pid, signal.SIGCONT)
                os.kill(pid, signal.SIGTERM)
                stop = time.monotonic() + 3
                while os.waitpid(pid, os.WNOHANG)[0] == 0:
                    if time.monotonic() >= stop:
                        os.kill(pid, signal.SIGKILL)
                        os.waitpid(pid, 0)
                        break
                    time.sleep(.02)
print("native host approval view passed")
