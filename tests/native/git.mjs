import assert from "node:assert/strict";
import { spawn, spawnSync } from "node:child_process";
import { randomBytes } from "node:crypto";
import { once } from "node:events";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { createServer } from "node:http";
import { dirname, join } from "node:path";
import { setTimeout as delay } from "node:timers/promises";

const [driver, slopbox, pi] = process.argv.slice(2);
assert(driver && slopbox && pi, "driver, Slopbox and Pi paths required");
const git = ["/opt/homebrew/bin/git", "/usr/local/bin/git"].find(existsSync);
assert(git && /\/Cellar\/git\//.test(realpathSync(git)), "Homebrew Git required");
const root = mkdtempSync("/private/var/tmp/slopbox-git-");
const workspace = join(root, "workspace");
const repository = join(root, "repositories/fixture.git");
const key = join(root, "private/key");
const agentSocket = join(root, "private/agent.sock");
const hostConfig = join(root, "config/slopbox/config.toml");
const remote = "ssh://git@forgejo.native.invalid/org/fixture.git";
const pushUrl = "https://forgejo.native.invalid/org/fixture.git";
const token = randomBytes(32).toString("hex");
const hostEnv = { PATH: "/usr/bin:/bin", GIT_CONFIG_NOSYSTEM: "1", GIT_CONFIG_GLOBAL: "/dev/null" };
const quote = (value) => `'${value.replaceAll("'", "'\\''")}'`;
function run(command, args, env = hostEnv) {
  const result = spawnSync(command, args, { env, encoding: "utf8", timeout: 15000 });
  assert.ifError(result.error);
  assert.equal(result.status, 0, result.stderr);
  return result.stdout.trim();
}
let agent;
let agentClosed;
let child;
let closed;
let broker;
let brokerError;
let pulls = 0;
const requests = [];
try {
  for (const path of ["workspace", "repositories", "private", "home", "config/slopbox", "data"]) {
    mkdirSync(join(root, path), { recursive: true, mode: 0o700 });
  }
  run("/usr/bin/ssh-keygen", ["-q", "-t", "ed25519", "-N", "", "-f", key]);
  const fingerprint = run("/usr/bin/ssh-keygen", ["-lf", `${key}.pub`, "-E", "sha256"]).split(/\s+/)[1];
  agent = spawn("/usr/bin/ssh-agent", ["-D", "-a", agentSocket], { env: hostEnv, stdio: "ignore" });
  agentClosed = once(agent, "close");
  const agentDeadline = Date.now() + 5000;
  while (!existsSync(agentSocket)) {
    assert.equal(agent.exitCode, null, "disposable SSH agent exited");
    assert(Date.now() < agentDeadline, "disposable SSH agent did not start");
    await delay(10);
  }
  run("/usr/bin/ssh-add", ["-q", key], { ...hostEnv, SSH_AUTH_SOCK: agentSocket });
  const allowed = join(root, "private/allowed-signers");
  writeFileSync(allowed, `native@example.invalid ${readFileSync(`${key}.pub`, "utf8")}`);

  run(git, ["init", "--bare", "--initial-branch=main", repository]);
  run(git, ["-C", repository, "config", "http.receivepack", "true"]);
  run(git, ["init", "--initial-branch=main", workspace]);
  run(git, ["-C", workspace, "-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid", "commit", "--allow-empty", "-m", "initial"]);
  run(git, ["-C", workspace, "push", repository, "main"]);
  const initial = run(git, ["-C", workspace, "rev-parse", "HEAD"]);
  const upstream = run(git, ["-C", repository, "-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid", "commit-tree", `${initial}^{tree}`, "-p", initial, "-m", "upstream"]);
  run(git, ["-C", repository, "update-ref", "refs/heads/main", upstream]);
  run(git, ["-C", workspace, "remote", "add", "origin", remote]);
  run(git, ["-C", workspace, "remote", "set-url", "--push", "origin", pushUrl]);
  run(git, ["-C", workspace, "config", "branch.main.remote", "origin"]);
  run(git, ["-C", workspace, "config", "branch.main.merge", "refs/heads/main"]);
  const original = readFileSync(join(workspace, ".git/config"));
  writeFileSync(join(workspace, "git-guest.mjs"), readFileSync(new URL("./git-guest.mjs", import.meta.url)));
  writeFileSync(join(workspace, "fixture.json"), JSON.stringify({ key, agentSocket, hostConfig, upstream, remote }));

  broker = createServer(async (request, response) => {
    try {
      const url = new URL(request.url, "http://fixture.invalid");
      const isGit = url.pathname.startsWith("/fixture.git/");
      const expected = isGit ? `Basic ${Buffer.from(`fixture:${token}`).toString("base64")}` : `token ${token}`;
      assert(request.headers.authorization === expected, "host authentication not injected");
      requests.push(`${request.method} ${request.url}`);
      const chunks = [];
      let length = 0;
      for await (const chunk of request) { length += chunk.length; assert(length < 1024 * 1024); chunks.push(chunk); }
      const body = Buffer.concat(chunks);
      if (isGit) {
        const cgi = spawn(git, ["http-backend"], {
          env: { ...hostEnv, GIT_PROJECT_ROOT: join(root, "repositories"), GIT_HTTP_EXPORT_ALL: "1",
            PATH_INFO: url.pathname, QUERY_STRING: url.search.slice(1), REQUEST_METHOD: request.method,
            CONTENT_TYPE: request.headers["content-type"] ?? "", CONTENT_LENGTH: String(body.length),
            HTTP_CONTENT_ENCODING: request.headers["content-encoding"] ?? "", REMOTE_USER: "fixture" },
          stdio: ["pipe", "pipe", "pipe"], timeout: 15000,
        });
        const cgiClosed = once(cgi, "close");
        const output = [];
        let stderr = "";
        cgi.stdout.on("data", (chunk) => output.push(chunk));
        cgi.stderr.setEncoding("utf8").on("data", (chunk) => { stderr += chunk; });
        cgi.stdin.end(body);
        assert.equal((await cgiClosed)[0], 0, stderr);
        const bytes = Buffer.concat(output);
        const end = bytes.indexOf("\r\n\r\n");
        assert(end > 0, "missing CGI headers");
        let status = 200;
        for (const line of bytes.subarray(0, end).toString().split("\r\n")) {
          const colon = line.indexOf(":");
          assert(colon > 0);
          const name = line.slice(0, colon);
          const value = line.slice(colon + 1).trim();
          if (name.toLowerCase() === "status") status = Number(value.split(" ")[0]);
          else response.setHeader(name, value);
        }
        response.writeHead(status).end(bytes.subarray(end + 4));
      } else if (request.method === "GET" && url.pathname === "/api/v1/user") {
        response.writeHead(200, { "content-type": "application/json" }).end(JSON.stringify({ login: "native-fixture" }));
      } else if (request.method === "GET" && url.pathname === "/api/v1/reflect") {
        response.writeHead(200, { "content-type": "application/json" }).end(JSON.stringify({ token }));
      } else if (request.method === "POST" && url.pathname === "/api/v1/repos/org/fixture/pulls") {
        assert.deepEqual(JSON.parse(body), { base: "main", head: "native-signed", title: "disposable PR" });
        run(git, ["-C", repository, "rev-parse", "--verify", "refs/heads/native-signed"]);
        assert.equal(++pulls, 1);
        response.writeHead(201, { "content-type": "application/json" }).end('{"number":1}');
      } else {
        throw new Error("unexpected upstream request");
      }
    } catch (error) {
      brokerError = error;
      response.writeHead(500).end("fixture failed");
    }
  });
  broker.listen(0, "127.0.0.1");
  await once(broker, "listening");
  const configuration = `[policy]
harness = "none"
network = "none"
credentials = "none"
[macos]
node = ${JSON.stringify(process.execPath)}
pi_cli = ${JSON.stringify(pi)}
[secrets.fixture]
source = "environment"
variable = "NATIVE_FIXTURE_FORGEJO_TOKEN"
[[http_routes]]
name = "git"
workspace = ${JSON.stringify(workspace)}
upstream = "https://forgejo.native.invalid/fixture.git"
methods = ["GET", "POST"]
allow_private_addresses = true
authentication = { type = "basic", username = "fixture", secret = "fixture" }
git_urls = ${JSON.stringify([remote, pushUrl])}
[[http_routes]]
name = "api"
workspace = ${JSON.stringify(workspace)}
upstream = "https://forgejo.native.invalid/api"
methods = ["GET", "POST"]
allow_private_addresses = true
authentication = { type = "token", secret = "fixture" }
`;
  for (const phase of ["signing", "routes-only"]) {
    const identity = phase === "signing" ? `\n[[git.identities]]\nworkspace = ${JSON.stringify(workspace)}\nname = "Native fixture"\nemail = "native@example.invalid"\nsigning_key_fingerprint = ${JSON.stringify(fingerprint)}\n` : "";
    writeFileSync(hostConfig, configuration + identity);
    child = spawn(driver, ["--exact", "native_cli_tests::native_cli_fixture", "--ignored", "--nocapture"], {
      cwd: workspace,
      env: { ...hostEnv, HOME: join(root, "home"), XDG_CONFIG_HOME: join(root, "config"), XDG_DATA_HOME: join(root, "data"),
        SSH_AUTH_SOCK: agentSocket, NATIVE_FIXTURE_FORGEJO_TOKEN: token, SLOPBOX_TEST_SLOPBOX: slopbox,
        SLOPBOX_TEST_ACCOUNT: `127.0.0.1:${broker.address().port}` },
      stdio: ["pipe", "pipe", "pipe"],
    });
    closed = once(child, "close");
    let stderr = "";
    let text = "";
    const events = [];
    child.stderr.setEncoding("utf8").on("data", (chunk) => { stderr += chunk; });
    child.stdout.setEncoding("utf8").on("data", (chunk) => {
      text += chunk;
      assert(text.length < 1024 * 1024);
      let index;
      while ((index = text.indexOf("\n")) !== -1) {
        const line = text.slice(0, index);
        text = text.slice(index + 1);
        if (line.startsWith("{")) events.push(JSON.parse(line));
      }
    });
    child.stdin.write(`${JSON.stringify({ id: phase, type: "bash", command: `${quote(process.execPath)} git-guest.mjs ${phase}` })}\n`);
    const deadline = Date.now() + 60000;
    while (!events.some((event) => event.type === "response" && event.id === phase)) {
      assert.ifError(brokerError);
      assert(child.exitCode === null && child.signalCode === null, stderr);
      assert(Date.now() < deadline, `Git fixture timed out: ${stderr}`);
      await delay(10);
    }
    const event = events.find((event) => event.type === "response" && event.id === phase);
    assert(event.success, JSON.stringify(event));
    assert.equal(event.data.exitCode, 0, `${event.data.output}\n${stderr}`);
    assert(event.data.output.includes(`native Git ${phase} passed`), event.data.output);
    assert(!event.data.output.includes(token) && !stderr.includes(token), "credential leaked");
    child.stdin.end();
    assert.equal((await closed)[0], 0, stderr);
    assert.ifError(brokerError);
    assert.deepEqual(readFileSync(join(workspace, ".git/config")), original);
    const state = JSON.parse(readFileSync(join(workspace, "signing-state.json"), "utf8"));
    for (const path of [state.helper, dirname(state.socket), state.config]) assert(!existsSync(path), "session signing state survived cleanup");
    assert.equal(run(git, ["-C", repository, "rev-parse", "refs/heads/native-signed"]), state.commit);
    run(git, ["-C", repository, "-c", "gpg.format=ssh", "-c", "gpg.ssh.program=/usr/bin/ssh-keygen", "-c", `gpg.ssh.allowedSignersFile=${allowed}`, "verify-commit", state.commit]);
  }
  assert(requests.some((request) => request.startsWith("POST /fixture.git/git-receive-pack")));
  assert(requests.some((request) => request.startsWith("POST /fixture.git/git-upload-pack")));
  assert(!requests.some((request) => request.startsWith("DELETE ") || request.includes("unconfigured")));
  assert.equal(pulls, 1);
  console.log("native Git signing and account routes passed");
} finally {
  if (child && child.exitCode === null && child.signalCode === null) { child.kill("SIGTERM"); await closed; }
  if (agent && agent.exitCode === null && agent.signalCode === null) { agent.kill("SIGTERM"); await agentClosed; }
  broker?.closeAllConnections();
  broker?.close();
  rmSync(root, { recursive: true, force: true });
}
