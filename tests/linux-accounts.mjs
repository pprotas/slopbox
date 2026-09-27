import assert from "node:assert/strict";
import { spawn, spawnSync } from "node:child_process";
import { randomBytes } from "node:crypto";
import { once } from "node:events";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { createServer } from "node:https";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { setTimeout as delay } from "node:timers/promises";

const [slopbox] = process.argv.slice(2);
assert(slopbox);
const root = mkdtempSync(join(tmpdir(), "slopbox-accounts-"));
const home = join(root, "home");
const config = join(home, "config/slopbox/config.toml");
const key = join(root, "signing-key");
const agentSocket = join(root, "agent.sock");
const ca = join(root, "upstream-ca.pem");
const token = randomBytes(32).toString("hex");
const env = {
  PATH: process.env.PATH, HOME: home, SHELL: process.env.SHELL,
  XDG_CONFIG_HOME: join(home, "config"), XDG_DATA_HOME: join(home, "data"),
  XDG_RUNTIME_DIR: join(root, "runtime"),
  SSL_CERT_FILE: ca, SSL_CERT_DIR: join(root, "empty-roots"),
  SSH_AUTH_SOCK: agentSocket, ACCOUNT_FIXTURE_TOKEN: token,
  GIT_CONFIG_NOSYSTEM: "1", GIT_CONFIG_GLOBAL: "/dev/null",
};
let agent;
let agentClosed;
let child;
let childClosed;
let server;
let upstreamError;
let passed = false;
const requests = [];
function run(command, args) {
  const result = spawnSync(command, args, { env, encoding: "utf8", timeout: 10000 });
  assert.ifError(result.error);
  assert.equal(result.status, 0, result.stderr.replaceAll(token, "[REDACTED]"));
  return result.stdout.trim();
}
try {
  mkdirSync(join(home, "config/slopbox"), { recursive: true, mode: 0o700 });
  mkdirSync(join(root, "empty-roots"));
  mkdirSync(env.XDG_RUNTIME_DIR, { mode: 0o700 });
  run("openssl", ["req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1", "-subj", "/CN=Disposable upstream CA", "-addext", "basicConstraints=critical,CA:TRUE", "-keyout", join(root, "upstream-ca.key"), "-out", ca]);
  run("openssl", ["req", "-newkey", "rsa:2048", "-nodes", "-subj", "/CN=Disposable upstream", "-keyout", join(root, "upstream.key"), "-out", join(root, "upstream.csr")]);
  writeFileSync(join(root, "upstream.ext"), "subjectAltName=IP:127.0.0.1\nbasicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature,keyEncipherment\nextendedKeyUsage=serverAuth\n");
  run("openssl", ["x509", "-req", "-in", join(root, "upstream.csr"), "-CA", ca, "-CAkey", join(root, "upstream-ca.key"), "-CAcreateserial", "-days", "1", "-extfile", join(root, "upstream.ext"), "-out", join(root, "upstream.pem")]);
  run("ssh-keygen", ["-q", "-t", "ed25519", "-N", "", "-f", key]);
  const fingerprint = run("ssh-keygen", ["-lf", `${key}.pub`, "-E", "sha256"]).split(/\s+/)[1];
  agent = spawn("ssh-agent", ["-D", "-a", agentSocket], { env, stdio: "ignore" });
  agentClosed = once(agent, "close");
  for (let attempt = 0; !existsSync(agentSocket); attempt++) {
    assert(attempt < 100, "fixture SSH agent failed to start");
    await delay(20);
  }
  run("ssh-add", [key]);
  server = createServer({ key: readFileSync(join(root, "upstream.key")), cert: readFileSync(join(root, "upstream.pem")) }, (request, response) => {
    try {
      assert(request.headers.authorization === `Bearer ${token}`, "missing host authentication");
      requests.push(`${request.method} ${request.url}`);
      assert.equal(request.url, "/api/reflect");
      response.end(token);
    } catch (error) {
      upstreamError = error;
      response.writeHead(500).end("fixture failed");
    }
  });
  server.listen(0, "127.0.0.1");
  await once(server, "listening");
  const origin = `https://127.0.0.1:${server.address().port}`;
  writeFileSync(config, `
[policy]
network = "none"
credentials = "none"
harness = "none"
[defaults]
git_identity = "agent"
accounts = ["forge"]
[[git.identities]]
id = "agent"
name = "POC Agent"
email = "agent@example.test"
signing_key_fingerprint = ${JSON.stringify(fingerprint)}
[secrets.forge]
source = "environment"
variable = "ACCOUNT_FIXTURE_TOKEN"
[[http_routes]]
name = "forge"
upstream = "${origin}/api"
methods = ["GET"]
proxy = true
allow_private_addresses = true
authentication = { type = "bearer", secret = "forge" }
[[workspaces]]
paths = [${JSON.stringify(join(root, "restricted"))}]
git_identity = false
accounts = []
`, { mode: 0o600 });
  const allowed = join(root, "allowed-signers");
  writeFileSync(allowed, `agent@example.test ${readFileSync(`${key}.pub`, "utf8")}`);
  for (const path of ["first", "unrelated/second", "restricted/third"]) {
    const workspace = join(root, path);
    mkdirSync(workspace, { recursive: true });
    run("git", ["init", "--quiet", "--initial-branch=main", workspace]);
    const disabled = path.startsWith("restricted/");
    writeFileSync(join(workspace, "account-fixture.json"), JSON.stringify({ config, key, agent: agentSocket, origin, disabled }));
    for (const file of ["account-client.mjs", "linux-account-probe.mjs"]) {
      writeFileSync(join(workspace, file), readFileSync(new URL(file, import.meta.url)));
    }
    const status = run(slopbox, ["status", "--workspace", workspace, "--verbose"]);
    assert(status.includes(disabled ? "No authenticated routes" : "Account TLS"), status);
    assert(status.includes(disabled ? "Signing       Not configured" : "POC Agent <agent@example.test>"), status);
    for (const setting of ["accounts", "git_identity"]) {
      assert(status.includes(`${setting}-source: ${disabled ? "host workspaces[0]" : "host defaults"}`), status);
    }
    child = spawn(slopbox, ["run", "--workspace", workspace, "--dev-env", "none", "--", "slopbox", "tool-run", "--network", "none", "--", "node", "linux-account-probe.mjs"], {
      env, stdio: ["ignore", "pipe", "pipe"], timeout: 30000,
    });
    childClosed = once(child, "close");
    let log = "";
    child.stdout.on("data", chunk => { log += chunk; });
    child.stderr.on("data", chunk => { log += chunk; });
    assert.equal((await childClosed)[0], 0, log.replaceAll(token, "[REDACTED]"));
    assert(!log.includes(token), "credential leaked into output");
    assert(log.includes(disabled ? "disabled account/signing checks passed" : "shared account and signing tools passed"), log);
    if (!disabled) {
      run("git", ["-C", workspace, "-c", "gpg.format=ssh", "-c", `gpg.ssh.allowedSignersFile=${allowed}`, "verify-commit", "HEAD"]);
      assert.equal(run("git", ["-C", workspace, "log", "-1", "--format=%an <%ae>"]), "POC Agent <agent@example.test>");
    }
    assert.equal(existsSync(join(workspace, ".slopbox.toml")), false);
  }
  assert.ifError(upstreamError);
  assert.deepEqual(requests, Array(6).fill("GET /api/reflect"));
  passed = true;
  console.log("linux shared identity/accounts, curl/Node TLS mediation and enforcement passed");
} finally {
  if (child && child.exitCode === null && child.signalCode === null) { child.kill("SIGTERM"); await childClosed; }
  if (server) await new Promise(resolve => server.close(resolve));
  if (agent && agent.exitCode === null && agent.signalCode === null) { agent.kill("SIGTERM"); await agentClosed; }
  if (passed) rmSync(root, { recursive: true, force: true });
  else console.error(`Retained disposable account fixture at ${root}`);
}
