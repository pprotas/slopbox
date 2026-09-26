import fcntl
import json
import os
from pathlib import Path
import pty
import select
import signal
import struct
import sys
import tempfile
import termios
import time

slopbox, node, pi = sys.argv[1:]
for termination, terminal_environment, truecolor in [
    (None, {"TERM": "xterm-256color", "COLORTERM": "truecolor"}, True),
    (signal.SIGTERM, {"TERM": "xterm-ghostty"}, True),
    (signal.SIGHUP, {"TERM": "xterm-256color"}, False),
]:
    with tempfile.TemporaryDirectory(prefix="slopbox-terminal-", dir="/private/var/tmp") as directory:
        root = Path(directory)
        for name in ["workspace", "home", "config/slopbox", "data"]:
            (root / name).mkdir(parents=True)
        (root / "config/slopbox/config.toml").write_text(
            f'[policy]\nharness="none"\n[macos]\nnode={json.dumps(node)}\npi_cli={json.dumps(pi)}\n'
        )
        environment = {
            "HOME": str(root / "home"), "XDG_CONFIG_HOME": str(root / "config"),
            "XDG_DATA_HOME": str(root / "data"), "PATH": "/usr/bin:/bin",
            **terminal_environment,
        }
        master, slave = pty.openpty()
        os.set_blocking(master, False)
        saved = termios.tcgetattr(slave)
        fcntl.ioctl(master, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 100, 0, 0))
        pid = os.fork()
        if pid == 0:
            os.setsid()
            fcntl.ioctl(slave, termios.TIOCSCTTY, 0)
            for target in [0, 1, 2]:
                os.dup2(slave, target)
            os.close(master)
            os.close(slave)
            os.chdir(root / "workspace")
            os.execve(slopbox, [slopbox, "run", "--approval-view", "--dev-env", "none", "--", "pi", "--provider", "openrouter", "--model", "openai/gpt-4o"], environment)
        output = bytearray()
        deadline = time.monotonic() + 25
        exited = False

        def pump():
            assert time.monotonic() < deadline, f"terminal timeout (termination={termination}):\n{output.decode(errors='replace')[-6000:]}"
            if select.select([master], [], [], .02)[0]:
                try:
                    chunk = os.read(master, 65536)
                except BlockingIOError:
                    return
                output.extend(chunk)
                assert len(output) < 2 * 1024 * 1024
                if b"\x1b[6n" in chunk:
                    os.write(master, b"\x1b[1;1R")
                if b"\x1b[c" in chunk:
                    os.write(master, b"\x1b[?1;2c")

        try:
            while b"slopbox.ts" not in output or b"openai/gpt-4o" not in output:
                pump()
            assert termios.tcgetattr(slave) != saved, "host terminal never entered raw mode"
            assert (b"\x1b[38;2;" in output) == truecolor, f"wrong terminal color mode: {terminal_environment}"
            before_view = len(output)
            os.write(master, b"\x1d")
            while b"SLOPBOX HOST APPROVALS" not in output[before_view:] or b"\r\n> " not in output[before_view:]:
                pump()
            if termination is None:
                before_return = len(output)
                os.write(master, b"q\n")
                while b"\x1b[?2026h" not in output[before_return:]:
                    pump()
                os.write(master, b"\x1a")
                while True:
                    stopped, status = os.waitpid(pid, os.WNOHANG | os.WUNTRACED)
                    if stopped:
                        assert os.WIFSTOPPED(status)
                        break
                    pump()
                assert termios.tcgetattr(master) == saved, "terminal not restored while suspended"
                os.kill(pid, signal.SIGCONT)
                while termios.tcgetattr(master) == saved:
                    pump()
                for prefix, number in [("!", 7313), ("!!", 9926)]:
                    os.write(master, f"{prefix}printf sandboxed > marker-{number}; printf 'shell-done-%s\\n' {number}\r".encode())
                    while f"shell-done-{number}".encode() not in output:
                        pump()
                    assert (root / f"workspace/marker-{number}").read_text() == "sandboxed"
                os.write(master, b"!/bin/sleep 30 & echo $! > cancel-pid; wait\r")
                while not (root / "workspace/cancel-pid").exists():
                    pump()
                descendant = int((root / "workspace/cancel-pid").read_text())
                os.write(master, b"\x1b")
                while True:
                    try:
                        os.kill(descendant, 0)
                    except ProcessLookupError:
                        break
                    pump()
                for columns in [60, 120, 80, 100, 80]:
                    fcntl.ioctl(master, termios.TIOCSWINSZ, struct.pack("HHHH", 24, columns, 0, 0))
                    pump()
                # Pi documents Ctrl+D as exit. Ctrl+C is editor cancellation
                # in Pi 0.85.1 and can leave the test parked at an empty prompt
                # after a bash cancellation.
                os.write(master, b"\x04")
            else:
                os.kill(pid, termination)
            while True:
                pump()
                finished, status = os.waitpid(pid, os.WNOHANG)
                if finished:
                    exited = True
                    expected = 0 if termination is None else 128 + termination
                    assert os.WIFEXITED(status) and os.WEXITSTATUS(status) == expected, (status, output.decode(errors="replace")[-6000:])
                    break
            assert termios.tcgetattr(master) == saved, "host terminal settings were not restored"
        finally:
            # A killed Darwin process can wait for unread PTY output to drain.
            os.close(master)
            os.close(slave)
            if not exited:
                os.kill(pid, signal.SIGTERM)
                stop = time.monotonic() + 3
                while os.waitpid(pid, os.WNOHANG)[0] == 0:
                    if time.monotonic() >= stop:
                        os.kill(pid, signal.SIGKILL)
                        os.waitpid(pid, 0)
                        break
                    time.sleep(.02)
print("native terminal !/!!, host approvals/redraw, Escape cancellation, Ctrl+D exit, resize, suspension, restoration and color detection passed")
