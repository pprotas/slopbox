"""Exercise unmodified native Claude Code through a disposable HTTPS account gateway."""

import http.server
import json
import os
import secrets
import shutil
import ssl
import subprocess
import sys
import tempfile
import threading
from pathlib import Path
from urllib.parse import urlsplit

native = sys.platform == "darwin"
claude = Path(sys.argv[2]).absolute()
assert claude.is_file()
if native:
    node = Path(sys.argv[3]).resolve()
    driver = Path(sys.argv[4]).resolve()
    bundles = []
    executables = [
        str(claude),
        str(node),
        "cat",
        "uname",
        "sleep",
        "chmod",
        "mkdir",
        "rm",
        "touch",
    ]
    probe_command = "node probe.mjs"
else:
    assert not Path("/nix").exists()
    stdlib = Path(
        subprocess.check_output(
            [
                "/usr/bin/python3",
                "-I",
                "-c",
                "import sysconfig; print(sysconfig.get_path('stdlib'))",
            ],
            text=True,
        ).strip()
    )
    bundles = [str(stdlib)]
    customization = Path("/etc") / stdlib.name
    if customization.is_dir():
        bundles.append(str(customization))
    executables = [
        str(claude),
        "git",
        "python3",
        "cat",
        "uname",
        "sleep",
        "chmod",
        "mkdir",
        "rm",
        "touch",
    ]
    probe_command = "python3 probe.py"

with tempfile.TemporaryDirectory(
    prefix="slopbox-claude-", dir="/private/var/tmp" if native else Path.home()
) as directory:
    root = Path(directory)
    home = root / "home"
    config = home / ".config/slopbox/config.toml"
    config.parent.mkdir(parents=True)
    (home / ".ssh").mkdir()
    (home / ".ssh/key").write_text("private-host-file")
    (home / ".claude").mkdir()
    (home / ".claude/.credentials.json").write_text("private-host-login")
    (root / "empty-roots").mkdir()
    (root / "runtime").mkdir()
    binary = root / "slopbox"
    shutil.copyfile(Path(sys.argv[1]).resolve(), binary)
    binary.chmod(0o755)
    token = secrets.token_hex(24)
    env = {
        "HOME": str(home),
        "PATH": os.environ.get("PATH", "/usr/bin:/bin:/usr/sbin:/sbin"),
        "XDG_RUNTIME_DIR": str(root / "runtime"),
        "FIXTURE_ACCOUNT_TOKEN": token,
        "OPENROUTER_API_KEY": "disposable-model-authority-canary",
        "SSL_CERT_FILE": str(root / "ca.pem"),
        "SSL_CERT_DIR": str(root / "empty-roots"),
    }
    if native:
        env["SLOPBOX_TEST_SLOPBOX"] = str(binary)
        env["SLOPBOX_TEST_TLS_CA"] = str(root / "ca.pem")
    stream_seen = threading.Event()
    failures = []

    def run(*args, cwd=root):
        output = []
        arguments = [str(arg) for arg in args]
        child_env = env.copy()
        if native and arguments[0] == str(binary):
            child_env["SLOPBOX_TEST_NATIVE_ARGS"] = json.dumps(arguments[1:])
            arguments = [
                str(driver),
                "native_cli_tests::native_generic_fixture",
                "--exact",
                "--ignored",
                "--nocapture",
            ]
        process = subprocess.Popen(
            arguments,
            env=child_env,
            cwd=cwd,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
        )

        def collect():
            for line in process.stdout:
                output.append(line)
                try:
                    message = json.loads(line)
                except json.JSONDecodeError:
                    continue
                if (
                    message.get("type") == "stream_event"
                    and message.get("event", {}).get("delta", {}).get("text")
                    == "stream-fixture-ready"
                ):
                    stream_seen.set()

        reader = threading.Thread(target=collect, daemon=True)
        reader.start()
        timed_out = False
        try:
            for _ in range(120):
                if failures or process.poll() is not None:
                    break
                try:
                    process.wait(timeout=1)
                except subprocess.TimeoutExpired:
                    continue
            timed_out = process.poll() is None and not failures
        finally:
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()
            reader.join(timeout=10)
            process.stdout.close()
        text = "".join(output)
        if timed_out and (cwd / "claude-debug.log").exists():
            text += (cwd / "claude-debug.log").read_text()
        assert token not in text and token not in str(failures), (
            "host credential leaked"
        )
        assert not failures, (text, failures)
        assert not timed_out, (text, failures)
        assert process.returncode == 0, (text, failures)
        return text

    run(
        "openssl",
        "req",
        "-x509",
        "-newkey",
        "rsa:2048",
        "-nodes",
        "-days",
        "1",
        "-subj",
        "/CN=Disposable Claude fixture CA",
        "-addext",
        "basicConstraints=critical,CA:TRUE",
        "-keyout",
        root / "ca.key",
        "-out",
        root / "ca.pem",
    )
    run(
        "openssl",
        "req",
        "-newkey",
        "rsa:2048",
        "-nodes",
        "-subj",
        "/CN=Disposable model",
        "-keyout",
        root / "server.key",
        "-out",
        root / "server.csr",
    )
    (root / "server.ext").write_text(
        "subjectAltName=IP:127.0.0.1\nbasicConstraints=critical,CA:FALSE\n"
        "keyUsage=critical,digitalSignature,keyEncipherment\nextendedKeyUsage=serverAuth\n"
    )
    run(
        "openssl",
        "x509",
        "-req",
        "-in",
        root / "server.csr",
        "-CA",
        root / "ca.pem",
        "-CAkey",
        root / "ca.key",
        "-CAcreateserial",
        "-days",
        "1",
        "-extfile",
        root / "server.ext",
        "-out",
        root / "server.pem",
    )
    requests = []
    step = 0
    before = ""

    class Model(http.server.BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.1"

        def do_POST(self):
            global step
            try:
                assert self.headers.get("Authorization") == f"Bearer {token}", (
                    "wrong upstream credential"
                )
                assert self.headers.get("anthropic-version") == "2023-06-01"
                assert urlsplit(self.path).path == "/v1/messages", self.path
                body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
                requests.append(body)
                if body["model"] == "fixture-account-probe":
                    reply = b'{"type":"message","content":[{"type":"text","text":"account-accessible"}]}'
                    self.send_response(200)
                    self.send_header("Content-Type", "application/json")
                    self.send_header("Content-Length", str(len(reply)))
                    self.end_headers()
                    self.wfile.write(reply)
                    return
                assert body["stream"] is True
                assert body["model"] == "claude-sonnet-4-6"
                assert {"Read", "Edit", "Bash"} <= {
                    tool["name"] for tool in body["tools"]
                }
                if step:
                    results = [
                        block
                        for block in body["messages"][-1]["content"]
                        if block["type"] == "tool_result"
                    ]
                    assert len(results) == 1, results
                    assert not results[0].get("is_error"), results
                    if step == 1:
                        assert before in str(results[0]["content"]), results
                    if step == 3:
                        assert "probe-complete" in str(results[0]["content"]), results
                calls = [
                    ("Read", {"file_path": str(workspace / "answer.txt")}),
                    (
                        "Edit",
                        {
                            "file_path": str(workspace / "answer.txt"),
                            "old_string": before,
                            "new_string": "claude-harness-passed",
                        },
                    ),
                    (
                        "Bash",
                        {
                            "command": probe_command,
                            "description": "Check Slopbox isolation",
                        },
                    ),
                ]
                self.send_response(200)
                self.send_header("Content-Type", "text/event-stream")
                self.send_header("Transfer-Encoding", "chunked")
                self.send_header("Connection", "close")
                self.end_headers()
                self.event(
                    "message_start",
                    message={
                        "id": f"msg_fixture_{step}",
                        "type": "message",
                        "role": "assistant",
                        "model": body["model"],
                        "content": [],
                        "stop_reason": None,
                        "stop_sequence": None,
                        "usage": {"input_tokens": 10, "output_tokens": 0},
                    },
                )
                self.event(
                    "content_block_start",
                    index=0,
                    content_block={"type": "text", "text": ""},
                )
                self.event(
                    "content_block_delta",
                    index=0,
                    delta={
                        "type": "text_delta",
                        "text": "stream-fixture-ready"
                        if step == 0
                        else "fixture-response",
                    },
                )
                if step == 0:
                    # Flush credential-redaction look-behind without finishing the response.
                    self.chunk(b": " + b"padding" * 128 + b"\n\n")
                    assert stream_seen.wait(15), (
                        "client did not receive SSE before response completion"
                    )
                self.event("content_block_stop", index=0)
                if step < len(calls):
                    name, inputs = calls[step]
                    self.event(
                        "content_block_start",
                        index=1,
                        content_block={
                            "type": "tool_use",
                            "id": f"toolu_fixture_{step}",
                            "name": name,
                            "input": {},
                        },
                    )
                    encoded = json.dumps(inputs)
                    for part in [
                        encoded[: len(encoded) // 2],
                        encoded[len(encoded) // 2 :],
                    ]:
                        self.event(
                            "content_block_delta",
                            index=1,
                            delta={"type": "input_json_delta", "partial_json": part},
                        )
                    self.event("content_block_stop", index=1)
                self.event(
                    "message_delta",
                    delta={
                        "stop_reason": "tool_use" if step < len(calls) else "end_turn",
                        "stop_sequence": None,
                    },
                    usage={"output_tokens": 10},
                )
                step += 1
                self.event("message_stop")
                self.wfile.write(b"0\r\n\r\n")
                self.wfile.flush()
            except (AssertionError, OSError, ValueError, KeyError) as error:
                failures.append(str(error))
                self.close_connection = True

        def event(self, kind, **fields):
            data = json.dumps({"type": kind, **fields})
            self.chunk(f"event: {kind}\ndata: {data}\n\n".encode())

        def chunk(self, data):
            self.wfile.write(f"{len(data):x}\r\n".encode() + data + b"\r\n")
            self.wfile.flush()

        def log_message(self, *args):
            pass

    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Model)
    context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    context.load_cert_chain(root / "server.pem", root / "server.key")
    server.socket = context.wrap_socket(server.socket, server_side=True)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    config.write_text(f"""[policy]
network = "none"
credentials = "brokered"
harness = "none"
[runtime]
executables = {json.dumps(executables)}
bundles = {json.dumps(bundles)}
[defaults]
accounts = ["fixture"]
[secrets.fixture]
source = "environment"
variable = "FIXTURE_ACCOUNT_TOKEN"
[[http_routes]]
name = "fixture"
upstream = "https://127.0.0.1:{server.server_port}"
methods = ["POST"]
allow_private_addresses = true
authentication = {{ type = "bearer", secret = "fixture" }}
""")
    try:
        for relative in ["first", "unrelated/second"]:
            workspace = root / relative
            workspace.mkdir(parents=True)
            before = secrets.token_hex(12)
            step = 0
            stream_seen.clear()
            (workspace / "answer.txt").write_text(before + "\n")
            (workspace / "probe.py").write_text(f"""
import json, os, socket, subprocess, sys, urllib.request
from pathlib import Path
inner = '--inner' in sys.argv
for path in [{str(config)!r}, {str(home / ".ssh/key")!r}, {str(home / ".claude/.credentials.json")!r}, {str(root / "ca.key")!r}, {str(root / "server.key")!r}]:
    assert not Path(path).exists(), path
assert not os.environ.get('FIXTURE_ACCOUNT_TOKEN')
assert not os.environ.get('SSH_AUTH_SOCK')
assert os.environ.get('OPENROUTER_API_KEY') is None
assert os.environ.get('ANTHROPIC_AUTH_TOKEN') in (None, 'slopbox:fixture')
with socket.socket(socket.AF_UNIX) as model:
    try: model.connect('/run/slopbox-host/model/gateway.sock')
    except OSError: assert inner, 'outer fixed-model broker is unreachable'
    else: assert not inner, 'inner reached the fixed-model broker'
if inner:
    request = urllib.request.Request('http://127.0.0.1:39082/fixture/v1/messages',
        data=b'{{"model":"fixture-account-probe","messages":[]}}',
        headers={{'Content-Type':'application/json', 'anthropic-version':'2023-06-01'}})
    with urllib.request.urlopen(request, timeout=5) as response:
        assert json.load(response)['content'][0]['text'] == 'account-accessible'
for host, port in [('1.1.1.1', 80), ('127.0.0.1', {server.server_port})]:
    try: socket.create_connection((host, port), timeout=1)
    except OSError: pass
    else: raise AssertionError('direct host/network access')
assert Path({str(claude)!r}).is_file(), 'selected executable missing'
try: os.chmod({str(claude)!r}, 0o755)
except OSError: pass
else: raise AssertionError('writable host executable')
if not inner:
    subprocess.run(['slopbox', 'tool-run', '--network', 'none', '--', 'python3', 'probe.py', '--inner'], check=True)
Path('inner-passed' if inner else 'outer-passed').write_text('passed')
print('probe-complete')
""")
            if native:
                shutil.copyfile(
                    Path(__file__).parent / "native/generic-probe.mjs",
                    workspace / "probe.mjs",
                )
                (workspace / "fixture.json").write_text(
                    json.dumps(
                        {
                            "hidden": [
                                str(config),
                                str(home / ".ssh/key"),
                                str(home / ".claude/.credentials.json"),
                                str(root / "ca.key"),
                                str(root / "server.key"),
                            ],
                            "executable": str(claude),
                            "upstreamPort": server.server_port,
                        }
                    )
                )
            version = run(
                binary,
                "run",
                "--workspace",
                workspace,
                "--dev-env",
                "none",
                "--",
                str(claude),
                "--version",
            )
            assert any(
                known in version for known in ("2.1.274 (Claude Code)", "2.1.283 (Claude Code)")
            ), version
            output = run(
                binary,
                "run",
                "--workspace",
                workspace,
                "--dev-env",
                "none",
                "--",
                "bash",
                "-c",
                'export ANTHROPIC_BASE_URL="${SLOPBOX_AUTHENTICATED_HTTP_BASE_URL}/fixture" CLAUDE_CODE_TMPDIR="$TMPDIR"; exec "$@"',
                "fixture",
                "env",
                "ANTHROPIC_AUTH_TOKEN=slopbox:fixture",
                "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1",
                "CLAUDE_CODE_DISABLE_OFFICIAL_MARKETPLACE_AUTOINSTALL=1",
                str(claude),
                "--debug-file",
                str(workspace / "claude-debug.log"),
                "-p",
                f"Read answer.txt, replace its contents with claude-harness-passed, then run {probe_command}.",
                "--model",
                "claude-sonnet-4-6",
                "--tools",
                "Read,Edit,Bash",
                "--allowedTools",
                "Read,Edit,Bash",
                "--permission-mode",
                "dontAsk",
                "--max-turns",
                "6",
                "--no-session-persistence",
                "--output-format",
                "stream-json",
                "--verbose",
                "--include-partial-messages",
                cwd=workspace,
            )
            assert not failures, failures
            assert step == 4 and stream_seen.is_set(), (step, output)
            results = [
                json.loads(line) for line in output.splitlines() if line.startswith("{")
            ]
            result = [item for item in results if item.get("type") == "result"]
            assert len(result) == 1 and not result[0]["is_error"], output
            assert (
                result[0]["num_turns"] == 4 and not result[0]["permission_denials"]
            ), output
            assert not any(item.get("is_api_error_message") for item in results), output
            assert (
                workspace / "answer.txt"
            ).read_text() == "claude-harness-passed\n", output
            assert (workspace / "outer-passed").is_file(), output
            assert (workspace / "outer-passed").read_text() == "passed", output
            if not native:
                assert (workspace / "inner-passed").is_file(), output
                assert (workspace / "inner-passed").read_text() == "passed", output
            assert not (workspace / ".slopbox.toml").exists()
        assert len(requests) == 10, len(requests)
        print(
            "PASS: Claude Code streamed, read, edited and ran Bash in two workspaces"
        )
        print(
            "PASS: verified HTTPS account mediation, credential containment and direct-network denial"
        )
        if native:
            print(
                "PASS: native shell retained outer account/model authority; no automatic tool separation"
            )
        else:
            print(
                "PASS: tool-run denied the fixed-model broker; account-scoped inference remained accessible"
            )
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)
