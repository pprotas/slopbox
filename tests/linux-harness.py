"""Run pinned, unmodified Aider from a dedicated venv against a disposable HTTPS model."""

import http.server
import json
import os
import secrets
import shlex
import shutil
import ssl
import subprocess
import sys
import tempfile
import threading
from pathlib import Path

assert not Path("/nix").exists()
venv = Path(sys.argv[2]).resolve()
python = venv / "bin/python"
python_prefix = subprocess.check_output(
    [python, "-I", "-c", "import sys; print(sys.base_prefix)"], text=True
).strip()
# A distro interpreter needs its stdlib; a dedicated interpreter can be one bundle.
stdlib = subprocess.check_output(
    [python, "-I", "-c", "import sysconfig; print(sysconfig.get_path('stdlib'))"],
    text=True,
).strip()
interpreter_bundle = (
    stdlib if python_prefix in {"/usr", "/usr/local"} else python_prefix
)
bundles = [str(venv), interpreter_bundle]
customization = Path("/etc") / Path(stdlib).name
if python_prefix == "/usr" and customization.is_dir():
    bundles.append(str(customization))
assert (venv / "tiktoken").is_dir(), "preseed the fixture tokenizer data in the bundle"
with tempfile.TemporaryDirectory(
    prefix="slopbox-harness-", dir=Path.home()
) as directory:
    root = Path(directory)
    home = root / "home"
    config = home / ".config/slopbox/config.toml"
    config.parent.mkdir(parents=True)
    (home / ".ssh").mkdir()
    (home / ".ssh/key").write_text("private-host-file")
    (root / "empty-roots").mkdir()
    (root / "runtime").mkdir()
    binary = root / "slopbox"
    shutil.copyfile(Path(sys.argv[1]).resolve(), binary)
    binary.chmod(0o755)
    token = secrets.token_hex(24)
    env = {
        "HOME": str(home),
        "PATH": os.environ["PATH"],
        "XDG_RUNTIME_DIR": str(root / "runtime"),
        "FIXTURE_ACCOUNT_TOKEN": token,
        "OPENROUTER_API_KEY": "disposable-model-authority-canary",
        "SSL_CERT_FILE": str(root / "ca.pem"),
        "SSL_CERT_DIR": str(root / "empty-roots"),
    }

    def run(*args, cwd=root):
        result = subprocess.run(
            [str(arg) for arg in args],
            env=env,
            cwd=cwd,
            capture_output=True,
            text=True,
            timeout=120,
            check=False,
        )
        output = result.stdout + result.stderr
        assert token not in output, "host credential leaked"
        assert result.returncode == 0, output
        return output

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
        "/CN=Disposable harness CA",
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

    class Model(http.server.BaseHTTPRequestHandler):
        def do_POST(self):
            authenticated = self.headers.get("Authorization") == f"Bearer {token}"
            body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            requests.append((self.path, authenticated, body))
            reply = json.dumps(
                {
                    "id": "chatcmpl-fixture",
                    "object": "chat.completion",
                    "created": 1,
                    "model": "gpt-4o",
                    "choices": [
                        {
                            "index": 0,
                            "finish_reason": "stop",
                            "message": {
                                "role": "assistant",
                                "content": "answer.txt\n```\nbundle-harness-passed\n```\n",
                            },
                        }
                    ],
                    "usage": {
                        "prompt_tokens": 10,
                        "completion_tokens": 10,
                        "total_tokens": 20,
                    },
                }
            ).encode()
            self.send_response(200 if authenticated else 401)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(reply)))
            self.end_headers()
            self.wfile.write(reply)

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
executables = [{json.dumps(str(venv / "bin/aider"))}, "git"]
bundles = {json.dumps(bundles)}
[defaults]
accounts = ["fixture"]
[secrets.fixture]
source = "environment"
variable = "FIXTURE_ACCOUNT_TOKEN"
[[http_routes]]
name = "fixture"
upstream = "https://127.0.0.1:{server.server_port}/v1"
methods = ["POST"]
allow_private_addresses = true
authentication = {{ type = "bearer", secret = "fixture" }}
""")
    try:
        for relative in ["first", "unrelated/second"]:
            workspace = root / relative
            workspace.mkdir(parents=True)
            (workspace / "answer.txt").write_text("before\n")
            (workspace / "probe.py").write_text(f"""
import json, os, socket, subprocess, sys, urllib.request
from pathlib import Path
inner = '--inner' in sys.argv
for path in [{str(config)!r}, {str(home / ".ssh/key")!r}, {str(root / "ca.key")!r}, {str(root / "server.key")!r}]:
    assert not Path(path).exists(), path
assert not os.environ.get('FIXTURE_ACCOUNT_TOKEN')
assert not os.environ.get('SSH_AUTH_SOCK')
assert os.environ.get('OPENROUTER_API_KEY') == (None if inner else 'slopbox:openrouter')
assert Path('/run/slopbox-host/model/gateway.sock').exists() == (not inner)
with socket.socket(socket.AF_UNIX) as model:
    try: model.connect('/run/slopbox-host/model/gateway.sock')
    except OSError:
        assert inner, 'outer fixed-model broker is unreachable'
    else:
        assert not inner, 'inner reached the fixed-model broker'
if inner:
    request = urllib.request.Request('http://127.0.0.1:39082/fixture/chat/completions',
        data=b'{{"model":"fixture-account-probe","messages":[]}}',
        headers={{'Content-Type':'application/json'}})
    with urllib.request.urlopen(request, timeout=5) as response:
        assert json.load(response)['object'] == 'chat.completion'
for host, port in [('1.1.1.1', 80), ('127.0.0.1', {server.server_port})]:
    try: socket.create_connection((host, port), timeout=1)
    except OSError: pass
    else: raise AssertionError('direct host/network access')
try: Path({str(venv / "guest-write")!r}).write_text('changed')
except OSError: pass
else: raise AssertionError('writable host bundle')
if not inner:
    subprocess.run(['slopbox', 'tool-run', '--network', 'none', '--', {str(python)!r}, 'probe.py', '--inner'], check=True)
Path('inner-passed' if inner else 'outer-passed').write_text('passed')
""")
            output = run(
                binary,
                "run",
                "--workspace",
                workspace,
                "--dev-env",
                "none",
                "--",
                "env",
                "LITELLM_LOCAL_MODEL_COST_MAP=True",
                f"TIKTOKEN_CACHE_DIR={venv}/tiktoken",
                "aider",
                "--model",
                "openai/gpt-4o",
                "--edit-format",
                "whole",
                "--openai-api-base",
                "http://127.0.0.1:39082/fixture",
                "--openai-api-key",
                "slopbox:fixture",
                "--no-git",
                "--no-pretty",
                "--no-stream",
                "--no-check-update",
                "--no-show-release-notes",
                "--no-show-model-warnings",
                "--no-analytics",
                "--no-auto-lint",
                "--auto-test",
                "--test-cmd",
                f"{shlex.quote(str(python))} probe.py",
                "--yes",
                "--message",
                "Replace answer.txt with bundle-harness-passed.",
                "answer.txt",
                cwd=workspace,
            )
            assert "Aider v0.86.2" in output, output
            assert (
                workspace / "answer.txt"
            ).read_text() == "bundle-harness-passed\n", output
            assert (workspace / "outer-passed").read_text() == "passed", output
            assert (workspace / "inner-passed").read_text() == "passed", output
            assert not (workspace / ".slopbox.toml").exists()
        assert len(requests) == 4, [(path, auth) for path, auth, _ in requests]
        assert [body["model"] for _, _, body in requests] == [
            "gpt-4o",
            "fixture-account-probe",
        ] * 2
        for path, authenticated, body in requests:
            assert path == "/v1/chat/completions" and authenticated
            if body["model"] == "gpt-4o":
                assert body.get("stream", False) is False
                assert any("before" in str(message) for message in body["messages"])
        print(
            "PASS: unmodified Aider edited two workspaces through verified HTTPS account mediation"
        )
        print(
            "PASS: tool-run denied the fixed-model broker; account-scoped inference remained accessible"
        )
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)
