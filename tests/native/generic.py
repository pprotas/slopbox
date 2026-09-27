"""Native generic-command enforcement using only disposable state and selected Node."""

import json
import os
import shutil
import socket
import subprocess
import sys
import tempfile
import threading
from pathlib import Path

binary = Path(sys.argv[1]).resolve()
source_node = Path(sys.argv[2]).resolve()
with tempfile.TemporaryDirectory(
    prefix="slopbox-generic-", dir="/private/var/tmp"
) as directory:
    root = Path(directory)
    for name in ["home/.config/slopbox", "home/.ssh", "workspace", "installation"]:
        (root / name).mkdir(parents=True)
    node = root / "installation/node"
    shutil.copyfile(source_node, node)
    node.chmod(0o755)
    (root / "tools").symlink_to(node.parent, target_is_directory=True)
    config = root / "home/.config/slopbox/config.toml"
    secret = root / "home/.ssh/key"
    secret.write_text("host-canary")
    host_socket = socket.socket(socket.AF_UNIX)
    host_socket.bind(str(root / "host.sock"))
    host_socket.listen()
    tcp = socket.socket()
    tcp.bind(("127.0.0.1", 0))
    tcp.listen()
    late_socket = socket.socket(socket.AF_UNIX)
    stop = threading.Event()

    def publish_socket():
        while not stop.wait(0.01):
            if (root / "workspace/ready").exists():
                late_socket.bind(str(root / "workspace/late.sock"))
                late_socket.listen()
                return

    publisher = threading.Thread(target=publish_socket)
    env = {
        "HOME": str(root / "home"),
        "PATH": "/usr/bin:/bin",
        "HOST_CANARY": "must-not-enter",
    }
    config.write_text(f"""[policy]
network="none"
credentials="none"
harness="none"
[runtime]
executables=[{json.dumps(str(root / "tools" / node.name))}]
""")
    (root / "workspace/escape").symlink_to(secret)
    (root / "workspace/unselected").symlink_to("/bin/echo")
    (root / "workspace/probe.mjs").write_text("""
import assert from 'node:assert/strict';
import * as fs from 'node:fs';
import {spawn, spawnSync} from 'node:child_process';
import {once} from 'node:events';
import * as net from 'node:net';
import path from 'node:path';
const fixture = JSON.parse(fs.readFileSync('fixture.json'));
for (const file of [fixture.secret, fixture.config, 'escape']) {
  assert.throws(() => fs.readFileSync(file), {code:'EPERM'});
}
assert.throws(() => fs.chmodSync(fixture.node, 0o755), {code:'EPERM'});
assert.throws(() => fs.openSync(fixture.node, 'r+'), {code:'EPERM'});
assert.throws(() => fs.linkSync(fixture.node, 'runtime-link'), {code:'EPERM'});
assert.throws(() => fs.readdirSync(path.dirname(fixture.node)), {code:'EPERM'});
assert.equal(spawnSync('/bin/echo', ['unselected']).error?.code, 'EPERM');
assert.equal(spawnSync('./unselected', ['aliased']).error?.code, 'EPERM');
fs.writeFileSync('unselected-script', '#!/bin/echo\\n', {mode:0o755});
assert.equal(spawnSync('./unselected-script').error?.code, 'EPERM');
for (const device of ['/dev/random', '/dev/urandom', '/dev/zero']) {
  const fd = fs.openSync(device, 'r');
  assert.equal(fs.readSync(fd, Buffer.alloc(16), 0, 16, null), 16);
  fs.closeSync(fd);
}
assert(!process.env.HOST_CANARY);
assert(!process.env.SSH_AUTH_SOCK);
assert(!process.env.SLOPBOX_NATIVE_SOCKET);
assert(!process.env.SLOPBOX_MODEL_PROXY_PORT);
assert.throws(() => process.kill(fixture.hostPid, 0), {code:'EPERM'});
process.kill(process.pid, 0);
const child = spawn(fixture.node, ['-e', 'console.log("ready"); setInterval(() => {}, 1000)']);
await once(child.stdout, 'data');
const exited = once(child, 'exit');
assert(child.kill('SIGTERM'));
assert.equal((await exited)[1], 'SIGTERM');
const connect = options => new Promise((resolve, reject) => {
  const socket = net.createConnection(options);
  socket.once('error', reject);
  socket.once('connect', () => { socket.destroy(); resolve(); });
  socket.setTimeout(2000, () => socket.destroy(new Error('connect timeout')));
});
await assert.rejects(connect({host:'127.0.0.1', port:fixture.port}), {code:'EPERM'});
await assert.rejects(connect({path:fixture.socket}), {code:'EPERM'});
const alias = path.join(process.env.HOME, 'escape.sock');
fs.symlinkSync(fixture.socket, alias);
await assert.rejects(connect({path:alias}), {code:'EPERM'});
const workspaceAlias = path.join(process.env.HOME, 'workspace');
fs.symlinkSync(process.cwd(), workspaceAlias);
for (const address of ['denied.sock', path.join(workspaceAlias, 'aliased.sock')]) {
  await assert.rejects(new Promise((resolve, reject) => {
    const server = net.createServer();
    server.once('error', reject);
    server.listen(address, () => { server.close(); resolve(); });
  }), {code:'EPERM'});
}
fs.writeFileSync('ready', 'ready');
for (let attempts = 0; !fs.existsSync('late.sock') && attempts < 500; attempts++) {
  await new Promise(resolve => setTimeout(resolve, 10));
}
assert(fs.lstatSync('late.sock').isSocket());
await assert.rejects(connect({path:path.resolve('late.sock')}), {code:'EPERM'});
await assert.rejects(connect({path:path.join(workspaceAlias, 'late.sock')}), {code:'EPERM'});
const linkedSocket = path.join(process.env.HOME, 'linked.sock');
try { fs.linkSync('late.sock', linkedSocket); }
catch (error) { assert(['EPERM', 'EXDEV', 'EOPNOTSUPP'].includes(error.code), error); }
if (fs.existsSync(linkedSocket)) {
  await assert.rejects(connect({path:linkedSocket}), {code:'EPERM'});
}
const movedSocket = path.join(process.env.HOME, 'moved.sock');
try { fs.renameSync('late.sock', movedSocket); }
catch (error) { assert(['EPERM', 'EXDEV', 'EOPNOTSUPP'].includes(error.code), error); }
if (fs.existsSync(movedSocket)) {
  await assert.rejects(connect({path:movedSocket}), {code:'EPERM'});
}
const own = path.join(process.env.HOME, 'own.sock');
await assert.rejects(new Promise((resolve, reject) => {
  const server = net.createServer();
  server.once('error', reject);
  server.listen(own, () => { server.close(); resolve(); });
}), {code:'EPERM'});
fs.writeFileSync('passed', 'generic-native-passed');
console.log('generic-native-passed');
""")
    (root / "workspace/fixture.json").write_text(
        json.dumps(
            {
                "node": str(node),
                "secret": str(secret),
                "config": str(config),
                "hostPid": os.getpid(),
                "socket": str(root / "host.sock"),
                "port": tcp.getsockname()[1],
            }
        )
    )
    publisher.start()
    try:
        result = subprocess.run(
            [str(binary), "run", "--dev-env", "none", "--", "node", "probe.mjs"],
            check=False,
            env=env,
            cwd=root / "workspace",
            stdin=subprocess.DEVNULL,
            capture_output=True,
            text=True,
            timeout=60,
        )
        assert result.returncode == 0, result.stdout + result.stderr
        assert (root / "workspace/passed").read_text() == "generic-native-passed"
        publisher.join(timeout=2)
        late_socket.close()
        for name in ["late.sock", "denied.sock", "aliased.sock"]:
            (root / "workspace" / name).unlink(missing_ok=True)
        readonly = config.read_text().replace(
            "[policy]\n", '[policy]\nworkspace="read-only"\n'
        )
        config.write_text(readonly)
        result = subprocess.run(
            [
                str(binary),
                "run",
                "--dev-env",
                "none",
                "--",
                "node",
                "-e",
                "require('node:assert/strict').throws(() => require('node:fs').writeFileSync('denied', 'no'), {code:'EPERM'});",
            ],
            check=False,
            env=env,
            cwd=root / "workspace",
            stdin=subprocess.DEVNULL,
            capture_output=True,
            text=True,
            timeout=60,
        )
        assert result.returncode == 0, result.stdout + result.stderr
        assert not (root / "workspace/denied").exists()
        result = subprocess.run(
            [
                str(binary),
                "run",
                "--dev-env",
                "none",
                "--",
                "node",
                "-e",
                "process.exit(7)",
            ],
            check=False,
            env=env,
            cwd=root / "workspace",
            stdin=subprocess.DEVNULL,
            capture_output=True,
            text=True,
            timeout=60,
        )
        assert result.returncode == 7, result.stdout + result.stderr
        node.chmod(0o4755)
        assert node.stat().st_mode & 0o4000
        result = subprocess.run(
            [str(binary), "run", "--dev-env", "none", "--", "node", "--version"],
            check=False,
            env=env,
            cwd=root / "workspace",
            stdin=subprocess.DEVNULL,
            capture_output=True,
            text=True,
            timeout=60,
        )
        assert result.returncode != 0 and "setuid/setgid" in result.stderr, (
            result.stdout + result.stderr
        )
        print(
            "PASS: native generic commands, credential/socket/alias/network/signal denials"
        )
        print("PASS: read-only workspace and command exit-status propagation")
    finally:
        stop.set()
        publisher.join(timeout=2)
        late_socket.close()
        host_socket.close()
        tcp.close()
