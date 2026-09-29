"""Run with an ordinary Linux build; no Nix installation or Python packages needed."""

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
import time
from pathlib import Path

assert not Path("/nix").exists(), "this fixture must run without Nix"
source_binary = Path(sys.argv[1]).resolve()
with tempfile.TemporaryDirectory(
    prefix="slopbox-nixless-", dir=Path.home()
) as directory:
    root = Path(directory)
    home = root / "home"
    workspace = root / "workspace"
    prefix = root / "unusual-installation"
    for path in [
        home / "config/slopbox",
        home / ".ssh",
        workspace,
        prefix / "bin",
        prefix / "lib",
        root / "empty-roots",
        root / "runtime",
        root / "launcher",
    ]:
        path.mkdir(parents=True)
    binary = root / "launcher/slopbox"
    # Cargo's build output has hard links; use an ordinary installed copy.
    shutil.copyfile(source_binary, binary)
    binary.chmod(0o755)
    config = home / "config/slopbox/config.toml"
    canary = root / "host-execution-canary"
    token = secrets.token_hex(24)
    env = {
        "PATH": os.environ["PATH"],
        "HOME": str(home),
        "XDG_CONFIG_HOME": str(home / "config"),
        "XDG_DATA_HOME": str(home / "data"),
        "XDG_RUNTIME_DIR": str(root / "runtime"),
        "GIT_CONFIG_NOSYSTEM": "1",
        "GIT_CONFIG_GLOBAL": "/dev/null",
        "ACCOUNT_FIXTURE_TOKEN": token,
        "OPENROUTER_API_KEY": "nixless-model-canary",
        "SSL_CERT_FILE": str(root / "ca.pem"),
        "SSL_CERT_DIR": str(root / "empty-roots"),
        "SSH_AUTH_SOCK": str(root / "agent.sock"),
    }

    def run(*args, success=True, cwd=workspace):
        result = subprocess.run(
            [str(arg) for arg in args],
            env=env,
            cwd=cwd,
            text=True,
            capture_output=True,
            timeout=30,
            check=False,
        )
        log = (result.stdout + result.stderr).replace(token, "[REDACTED]")
        assert (result.returncode == 0) == success, log
        assert token not in result.stdout + result.stderr, (
            "credential leaked into command output"
        )
        return result.stdout if success else log

    (root / "inner.c").write_text("int inner(void) { return 42; }\n")
    (root / "outer.c").write_text(
        "int inner(void); int outer(void) { return inner() + 1; }\n"
    )
    (root / "tool.c").write_text(f"""
#include <stdio.h>
#include <unistd.h>
int outer(void);
int main(void) {{
    FILE *f = fopen({json.dumps(str(canary))}, "w");
    if (f) {{ fputs("executed on host", f); fclose(f); }}
    if (unlink("/run/slopbox/bwrap") == 0) return 99;
    printf("closure=%d\\n", outer());
    return 0;
}}
""")
    run("cc", "-shared", "-fPIC", root / "inner.c", "-o", prefix / "lib/libinner.so")
    run(
        "cc",
        "-shared",
        "-fPIC",
        root / "outer.c",
        f"-L{prefix}/lib",
        "-linner",
        "-Wl,-rpath,$ORIGIN",
        "-o",
        prefix / "lib/libouter.so",
    )
    run(
        "cc",
        root / "tool.c",
        f"-L{prefix}/lib",
        "-louter",
        "-Wl,-rpath,$ORIGIN/../lib",
        "-o",
        prefix / "bin/unfamiliar",
    )
    script = prefix / "bin/script-tool"
    script.write_text("#!/usr/bin/env bash\nset -eu\nprintf 'script-passed\\n'\n")
    script.chmod(0o755)
    (prefix / "lib/unrelated-data").write_text("not a runtime dependency")
    (home / ".ssh/key-canary").write_text("private-host-data")
    moved = root / "relocated-runtime"
    prefix.rename(moved)
    prefix = moved
    script = prefix / "bin/script-tool"
    tools = ["curl", "git", "findmnt", str(prefix / "bin/unfamiliar"), str(script)]
    base = (
        """[policy]
network = "none"
credentials = "none"
harness = "none"
[runtime]
executables = """
        + json.dumps(tools)
        + "\ndependency_roots = "
        + json.dumps([str(prefix)])
        + "\n"
    )
    config.write_text(base.replace(json.dumps([str(prefix)]), "[]"))
    error = run(
        binary,
        "run",
        "--workspace",
        workspace,
        "--dev-env",
        "none",
        "--",
        "unfamiliar",
        success=False,
    )
    assert "outside host-authorized roots" in error
    config.write_text(base)
    status = run(binary, "status", "--workspace", workspace, "--verbose")
    assert "runtime-source: host runtime.executables" in status
    assert "no automatic harness/tool separation" in status
    error = run(binary, success=False)
    assert "no default_command configured" in error, error
    assert "unexpected argument 'init'" in run(
        binary,
        "init",
        "--workspace",
        workspace,
        "--changes",
        "live",
        "--yes",
        success=False,
    )
    assert not (home / "data").exists()
    assert "0 failed check(s)" in run(binary, "doctor", "--workspace", workspace)
    assert not (home / "data").exists()
    assert not canary.exists(), "discovery executed a selected program"
    probe = workspace / "probe.sh"
    probe.write_text(f"""set -eu
unfamiliar
script-tool
curl --version >/dev/null
test ! -e /usr/bin/id
test ! -e /usr/bin/cc
test ! -e {prefix}/lib/unrelated-data
test ! -e {home}/.ssh/key-canary
test ! -e {config}
test ! -e /run/slopbox-host/model/gateway.sock
test -z "${{OPENROUTER_API_KEY-}}${{ACCOUNT_FIXTURE_TOKEN-}}${{SSH_AUTH_SOCK-}}"
for path in /run/slopbox /usr/bin/curl {prefix}/lib/libinner.so; do
    case ",$(findmnt -n -o VFS-OPTIONS -T "$path")," in *,ro,*) ;; *) exit 1;; esac
done
if printf changed >>{prefix}/lib/libinner.so 2>/dev/null; then exit 1; fi
if curl --silent --max-time 1 --noproxy '*' http://1.1.1.1 >/dev/null; then exit 1; fi
printf 'isolation-passed\\n'
""")
    output = run(
        binary,
        "run",
        "--workspace",
        workspace,
        "--dev-env",
        "none",
        "--",
        "/bin/sh",
        "-eu",
        "-c",
        ". ./probe.sh; slopbox tool-run --network none -- /bin/sh -eu ./probe.sh",
    )
    assert output.count("closure=43") == 2 and output.count("isolation-passed") == 2
    assert not canary.exists()

    config.write_text(
        'default_command = ["bash"]\n'
        + base
        + '\n[environment]\nCLIENT_STATE = "${HOME}/state"\nCLIENT_LITERAL = "$(false)"\n'
    )
    (workspace / "flake.nix").write_text("this must not be evaluated")
    assert "default-environment-passed" in run(
        binary,
        "--",
        "-eu",
        "-c",
        'test "$CLIENT_STATE" = "$HOME/state"; test "$CLIENT_LITERAL" = \'$(false)\'; printf default-environment-passed',
    )
    (workspace / "flake.nix").unlink()
    config.write_text(base)

    marker = root / "credential-command-ran"
    config.write_text(
        base
        + f"""
[defaults]
accounts = ["check"]
[secrets.check]
source = "command"
argv = ["sh", "-c", "printf called > {marker}; printf disposable"]
[[http_routes]]
name = "check"
upstream = "https://example.invalid/api"
methods = ["GET"]
authentication = {{ type = "bearer", secret = "check" }}
"""
    )
    run(
        binary,
        "run",
        "--workspace",
        workspace,
        "--dev-env",
        "none",
        "--",
        "/bin/sh",
        "-c",
        "exit 0",
    )
    assert marker.read_text() == "called"
    marker.unlink()
    library = prefix / "lib/libinner.so"
    library.rename(library.with_suffix(".missing"))
    error = run(
        binary,
        "run",
        "--workspace",
        workspace,
        "--dev-env",
        "none",
        "--",
        "unfamiliar",
        success=False,
    )
    assert "unresolved runtime dependency libinner.so" in error
    assert not marker.exists(), "failed runtime preparation resolved a credential"
    library.with_suffix(".missing").rename(library)
    config.write_text(base)
    private_library = root / "unapproved-library.so"
    shutil.copyfile(library, private_library)
    library.rename(library.with_suffix(".saved"))
    library.symlink_to(private_library)
    error = run(
        binary,
        "run",
        "--workspace",
        workspace,
        "--dev-env",
        "none",
        "--",
        "unfamiliar",
        success=False,
    )
    assert "outside host-authorized roots" in error
    library.unlink()
    library.with_suffix(".saved").rename(library)

    # RUNPATH is not inherited; RPATH is. Neither discovery path executes the tool.
    run(
        "cc",
        "-shared",
        "-fPIC",
        root / "outer.c",
        f"-L{prefix}/lib",
        "-linner",
        "-o",
        prefix / "lib/libouter.so",
    )
    error = run(
        binary,
        "run",
        "--workspace",
        workspace,
        "--dev-env",
        "none",
        "--",
        "unfamiliar",
        success=False,
    )
    assert "unresolved runtime dependency libinner.so" in error
    run(
        "cc",
        root / "tool.c",
        f"-L{prefix}/lib",
        "-louter",
        f"-Wl,-rpath-link,{prefix}/lib",
        "-Wl,--disable-new-dtags,-rpath,$ORIGIN/../lib",
        "-o",
        prefix / "bin/unfamiliar",
    )
    assert (
        run(
            binary,
            "run",
            "--workspace",
            workspace,
            "--dev-env",
            "none",
            "--",
            "unfamiliar",
        )
        == "closure=43\n"
    )
    os.link(prefix / "bin/unfamiliar", workspace / "writable-alias")
    error = run(
        binary,
        "run",
        "--workspace",
        workspace,
        "--dev-env",
        "none",
        "--",
        "unfamiliar",
        success=False,
    )
    assert "mutable hard-link aliases" in error
    (workspace / "writable-alias").unlink()
    script.write_text("#!/usr/bin/env python3\nprint('not executed')\n")
    error = run(
        binary,
        "run",
        "--workspace",
        workspace,
        "--dev-env",
        "none",
        "--",
        "script-tool",
        success=False,
    )
    assert "interpreter python3 is not selected" in error
    script.write_text("#!/usr/bin/env bash\nprintf 'script-passed\\n'\n")
    for forbidden in [workspace / "tool", home / ".ssh/tool"]:
        shutil.copyfile(prefix / "bin/unfamiliar", forbidden)
        forbidden.chmod(0o755)
        config.write_text(base.replace(json.dumps(tools), json.dumps([str(forbidden)])))
        error = run(
            binary,
            "run",
            "--workspace",
            workspace,
            "--dev-env",
            "none",
            "--",
            "/bin/sh",
            "-c",
            "exit 99",
            success=False,
        )
        assert "runtime overlaps" in error
    config.write_text(base)
    (workspace / ".slopbox.toml").write_text('[runtime]\nexecutables = ["python3"]\n')
    assert "unknown field" in run(
        binary, "status", "--workspace", workspace, success=False
    )
    (workspace / ".slopbox.toml").unlink()
    error = run(
        binary,
        "run",
        "--workspace",
        workspace,
        "--profile",
        "contained",
        "--",
        "/bin/sh",
        success=False,
    )
    assert "no fallback" in error

    # A deliberately cooperative command uses the existing stronger tool boundary.
    config.write_text(base.replace('credentials = "none"', 'credentials = "brokered"'))
    output = run(
        binary,
        "run",
        "--workspace",
        workspace,
        "--dev-env",
        "none",
        "--",
        "/bin/sh",
        "-eu",
        "-c",
        """
test -z "${OPENROUTER_API_KEY-}"
test -S /run/slopbox-host/model/gateway.sock
slopbox tool-run --network none -- /bin/sh -eu -c '
    test -z "${OPENROUTER_API_KEY-}${SLOPBOX_MODEL_PROXY_PORT-}"
    test ! -e /run/slopbox-host/model/gateway.sock
    printf separation-passed
'
""",
    )
    assert output == "separation-passed"

    config.write_text(
        base.replace('network = "none"', 'workspace = "staged"\nnetwork = "none"')
    )
    run(
        binary,
        "run",
        "--workspace",
        workspace,
        "--dev-env",
        "none",
        "--",
        "/bin/sh",
        "-c",
        "printf staged > changed",
    )
    assert not (workspace / "changed").exists()
    stages = list((home / "data/slopbox/boxes").glob("*/stages/*"))
    assert len(stages) == 1
    assert "+staged" in run(
        binary, "stage", "diff", stages[0].name, "--workspace", workspace
    )
    config.write_text(base)

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
        "/CN=Disposable upstream CA",
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
        "/CN=Disposable upstream",
        "-keyout",
        root / "upstream.key",
        "-out",
        root / "upstream.csr",
    )
    (root / "upstream.ext").write_text(
        "subjectAltName=IP:127.0.0.1\nbasicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature,keyEncipherment\nextendedKeyUsage=serverAuth\n"
    )
    run(
        "openssl",
        "x509",
        "-req",
        "-in",
        root / "upstream.csr",
        "-CA",
        root / "ca.pem",
        "-CAkey",
        root / "ca.key",
        "-CAcreateserial",
        "-days",
        "1",
        "-extfile",
        root / "upstream.ext",
        "-out",
        root / "upstream.pem",
    )
    requests = []

    class Upstream(http.server.BaseHTTPRequestHandler):
        def do_GET(self):
            authenticated = self.headers.get("Authorization") == f"Bearer {token}"
            requests.append((self.path, authenticated))
            response = token.encode() if authenticated else b"missing authentication"
            self.send_response(200 if authenticated else 401)
            self.send_header("Content-Length", str(len(response)))
            self.end_headers()
            self.wfile.write(response)

        def log_message(self, *args):
            pass

    while True:
        server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Upstream)
        if server.server_port not in {39080, 39081, 39082}:
            break
        server.server_close()
    context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    context.load_cert_chain(root / "upstream.pem", root / "upstream.key")
    server.socket = context.wrap_socket(server.socket, server_side=True)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    origin = f"https://127.0.0.1:{server.server_port}"
    connection_probe = f"curl --silent --max-time 1 --noproxy '*' --output /dev/null --write-out '%{{remote_ip}}' http://127.0.0.1:{server.server_port} || :"
    assert run("sh", "-c", connection_probe) == "127.0.0.1"
    key = root / "signing-key"
    run("ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-f", key)
    fingerprint = run("ssh-keygen", "-lf", f"{key}.pub", "-E", "sha256").split()[1]
    agent = subprocess.Popen(
        ["ssh-agent", "-D", "-a", env["SSH_AUTH_SOCK"]],
        env=env,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    try:
        for attempt in range(100):
            if Path(env["SSH_AUTH_SOCK"]).exists():
                break
            time.sleep(0.02)
        run("ssh-add", key)
        allowed = root / "allowed-signers"
        allowed.write_text("agent@example.test " + Path(f"{key}.pub").read_text())
        config.write_text(
            base
            + f'''
[defaults]
accounts = ["forge"]
git_identity = "agent"
[secrets.forge]
source = "environment"
variable = "ACCOUNT_FIXTURE_TOKEN"
[[http_routes]]
name = "forge"
upstream = "{origin}/api"
methods = ["GET"]
proxy = true
allow_private_addresses = true
authentication = {{ type = "bearer", secret = "forge" }}
[[git.identities]]
id = "agent"
name = "POC Agent"
email = "agent@example.test"
signing_key_fingerprint = "{fingerprint}"
[[workspaces]]
paths = [{json.dumps(str(root / "restricted"))}]
accounts = []
git_identity = false
'''
        )
        for relative in ["first", "unrelated/second", "restricted/third"]:
            project = root / relative
            project.mkdir(parents=True)
            run("git", "init", "--quiet", "--initial-branch=main", project)
            if relative.startswith("restricted"):
                command = """test -z "${SLOPBOX_ACCOUNT_CA-}${SLOPBOX_ACCOUNT_PROXY-}"
test ! -e /run/slopbox/git-sign
test ! -e /run/slopbox-host/authenticated-http/gateway.sock
printf disabled-passed
"""
            else:
                command = f"""
test -z "${{ACCOUNT_FIXTURE_TOKEN-}}${{SSH_AUTH_SOCK-}}${{OPENROUTER_API_KEY-}}"
for path in {key} {env["SSH_AUTH_SOCK"]} {config} /run/slopbox-host/model/gateway.sock; do
    test ! -e "$path"
done
case ",$(findmnt -n -o VFS-OPTIONS -T "$SLOPBOX_ACCOUNT_CA")," in *,ro,*) ;; *) exit 1;; esac
result=$(curl --silent --show-error --fail --max-time 5 --noproxy '' --proxy "$SLOPBOX_ACCOUNT_PROXY" --cacert "$SLOPBOX_ACCOUNT_CA" {origin}/api/reflect)
test "$result" = '[REDACTED]'
if curl --silent --fail --max-time 3 --noproxy '' --proxy "$SLOPBOX_ACCOUNT_PROXY" {origin}/api/reflect; then exit 1; fi
if curl --silent --fail --max-time 3 --noproxy '' --proxy "$SLOPBOX_ACCOUNT_PROXY" --cacert "$SLOPBOX_ACCOUNT_CA" {origin}/outside; then exit 1; fi
direct=$({connection_probe})
test -z "$direct"
git commit --quiet --allow-empty -m 'Nix-free signing fixture'
printf account-passed
"""
            output = run(
                binary,
                "run",
                "--workspace",
                project,
                "--dev-env",
                "none",
                "--",
                "slopbox",
                "tool-run",
                "--network",
                "none",
                "--",
                "/bin/sh",
                "-eu",
                "-c",
                command,
            )
            assert output == (
                "disabled-passed"
                if relative.startswith("restricted")
                else "account-passed"
            )
            if not relative.startswith("restricted"):
                run(
                    "git",
                    "-C",
                    project,
                    "-c",
                    "gpg.format=ssh",
                    "-c",
                    f"gpg.ssh.allowedSignersFile={allowed}",
                    "verify-commit",
                    "HEAD",
                )
            assert not (project / ".slopbox.toml").exists()
        assert requests == [("/api/reflect", True)] * 2, requests
    finally:
        agent.terminate()
        agent.wait(timeout=5)
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)
    assert not canary.exists()
    assert not Path("/nix").exists()
    print(
        "Nix-free ELF/script runtime, isolation, staging, shared accounts and signing passed"
    )
