import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { randomBytes } from "node:crypto";
import { once } from "node:events";
import { mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { createServer } from "node:http";
import { dirname, join } from "node:path";

const [driver, slopbox, ghPath] = process.argv.slice(2);
assert(driver && slopbox && ghPath, "test driver, built Slopbox and reviewed gh paths required");
const gh = realpathSync(ghPath);
const root = mkdtempSync("/private/var/tmp/slopbox-gh-");
const workspace = join(root, "workspace");
const token = randomBytes(32).toString("hex");
const ghConfig = join(root, "private-gh");
const credentials = join(ghConfig, "hosts.yml");
let child;
let closed;
let upstreamError;
let passed = false;
let log = "";
const requests = [];
const server = createServer(async (request, response) => {
  try {
    assert(request.headers.authorization === `Bearer ${token}`, "host credential was not injected");
    requests.push(`${request.method} ${request.url}`);
    response.setHeader("Content-Type", "application/json");
    if (request.url === "/api/v3/user") response.end('{"login":"fixture"}');
    else if (request.url === "/api/v3/reflect") response.end(JSON.stringify({ token }));
    else if (request.url === "/api/graphql") {
      let body = "";
      for await (const chunk of request) { body += chunk; assert(body.length < 65536); }
      assert.match(JSON.parse(body).query, /repository/);
      response.end('{"data":{"repository":{"nameWithOwner":"owner/repository"}}}');
    } else throw new Error("unexpected upstream request");
  } catch (error) {
    upstreamError = error;
    response.writeHead(500).end("fixture failed");
  }
});
try {
  for (const directory of [workspace, ghConfig, join(root, "home"), join(root, "data"), join(root, "config/slopbox")]) {
    mkdirSync(directory, { recursive: true, mode: 0o700 });
  }
  writeFileSync(credentials, `github.native.invalid:\n  user: fixture\n  oauth_token: ${token}\n  users:\n    fixture:\n      oauth_token: ${token}\n`, { mode: 0o600 });

  writeFileSync(join(workspace, "github-probe.mjs"), readFileSync(new URL("./github-probe.mjs", import.meta.url)));
  writeFileSync(join(workspace, "github-fixture.json"), JSON.stringify({ gh, credentials }));
  server.listen(0, "127.0.0.1");
  await once(server, "listening");
  const upstream = `127.0.0.1:${server.address().port}`;
  writeFileSync(join(root, "config/slopbox/config.toml"), `
[policy]
harness = "none"
network = "none"
credentials = "none"
[runtime]
executables = ${JSON.stringify([gh, process.execPath])}
[secrets.github]
source = "command"
argv = ["gh", "auth", "token", "--hostname", "github.native.invalid", "--user", "fixture"]
[[http_routes]]
name = "github"
workspace = ${JSON.stringify(workspace)}
upstream = "https://github.native.invalid/api"
methods = ["GET", "POST"]
direct = true
allow_private_addresses = true
authentication = { type = "bearer", secret = "github" }
`, { mode: 0o600 });
  child = spawn(driver, ["native_cli_tests::native_cli_fixture", "--exact", "--ignored", "--nocapture"], {
    cwd: workspace,
    env: { HOME: join(root, "home"), XDG_CONFIG_HOME: join(root, "config"), XDG_DATA_HOME: join(root, "data"),
      PATH: `${dirname(gh)}:/usr/bin:/bin`, GH_CONFIG_DIR: ghConfig,
      SLOPBOX_TEST_SLOPBOX: slopbox, SLOPBOX_TEST_ACCOUNT: upstream,
      SLOPBOX_TEST_NATIVE_ARGS: JSON.stringify(["run", "--workspace", workspace, "--dev-env", "none", "--", process.execPath, "github-probe.mjs"]),
      GITHUB_HOST_CANARY: "must-not-be-imported" },
    stdio: ["ignore", "pipe", "pipe"], timeout: 60000,
  });
  closed = once(child, "close");
  child.stdout.on("data", chunk => { log += chunk; });
  child.stderr.on("data", chunk => { log += chunk; });
  assert.equal((await closed)[0], 0, log);
  assert.ifError(upstreamError);
  assert(log.includes("native gh tool checks passed"), log);
  assert(!log.includes(token), "host credential appeared in output");
  assert.deepEqual(requests, ["GET /api/v3/user", "POST /api/graphql", "GET /api/v3/reflect"]);
  passed = true;
  console.log("native gh REST/GraphQL, host credential command and account denials passed");
} finally {
  if (child && child.exitCode === null && child.signalCode === null) { child.kill("SIGTERM"); await closed; }
  await new Promise(resolve => server.close(resolve));
  if (passed) rmSync(root, { recursive: true, force: true });
  else console.error(`Retained GitHub fixture at ${root}\n${log.replaceAll(token, "[REDACTED]")}`);
}
