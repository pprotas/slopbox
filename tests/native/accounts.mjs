import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { randomBytes } from "node:crypto";
import { once } from "node:events";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { createServer } from "node:http";
import { join } from "node:path";

const [driver, slopbox] = process.argv.slice(2);
assert(driver && slopbox);
const root = mkdtempSync("/private/var/tmp/slopbox-accounts-");
const token = randomBytes(32).toString("hex");
const config = join(root, "config/slopbox/config.toml");
const requests = [];
let upstreamError;
let child;
let closed;
let log = "";
let passed = false;
// The test supervisor pins this disposable HTTP fixture; TLS verification is covered by protocol tests.
const server = createServer((request, response) => {
  try {
    assert(request.headers.authorization === `Bearer ${token}`);
    requests.push(`${request.method} ${request.url}`);
    assert.equal(request.url, "/api/reflect");
    response.end(token);
  } catch (error) {
    upstreamError = error;
    response.writeHead(500).end("fixture failed");
  }
});
try {
  for (const path of ["first", "unrelated/second", "home", "data", "config/slopbox"]) {
    mkdirSync(join(root, path), { recursive: true, mode: 0o700 });
  }

  server.listen(0, "127.0.0.1");
  await once(server, "listening");
  writeFileSync(config, `
[policy]
harness = "none"
network = "none"
credentials = "none"
[runtime]
executables = ${JSON.stringify([process.execPath, "/usr/bin/curl"])}
[defaults]
accounts = ["forge"]
[secrets.forge]
source = "environment"
variable = "ACCOUNT_FIXTURE_TOKEN"
[[http_routes]]
name = "forge"
upstream = "https://forgejo.native.invalid/api"
methods = ["GET"]
proxy = true
allow_private_addresses = true
authentication = { type = "bearer", secret = "forge" }
`, { mode: 0o600 });
  for (const path of ["first", "unrelated/second"]) {
    const workspace = join(root, path);
    writeFileSync(join(workspace, "accounts-probe.mjs"), readFileSync(new URL("./accounts-probe.mjs", import.meta.url)));
    writeFileSync(join(workspace, "account-client.mjs"), readFileSync(new URL("../account-client.mjs", import.meta.url)));
    writeFileSync(join(workspace, "accounts-fixture.json"), JSON.stringify({ config, upstreamPort: server.address().port }));
    child = spawn(driver, ["native_cli_tests::native_cli_fixture", "--exact", "--ignored", "--nocapture"], {
      cwd: workspace,
      env: { HOME: join(root, "home"), XDG_CONFIG_HOME: join(root, "config"), XDG_DATA_HOME: join(root, "data"),
        PATH: "/usr/bin:/bin", ACCOUNT_FIXTURE_TOKEN: token,
        SLOPBOX_TEST_SLOPBOX: slopbox, SLOPBOX_TEST_ACCOUNT: `127.0.0.1:${server.address().port}`,
        SLOPBOX_TEST_NATIVE_ARGS: JSON.stringify(["run", "--workspace", workspace, "--dev-env", "none", "--", process.execPath, "accounts-probe.mjs"]) },
      stdio: ["ignore", "pipe", "pipe"], timeout: 60000,
    });
    closed = once(child, "close");
    child.stdout.on("data", chunk => { log += chunk; });
    child.stderr.on("data", chunk => { log += chunk; });
    assert.equal((await closed)[0], 0, log.replaceAll(token, "[REDACTED]"));
    assert.ifError(upstreamError);
  }
  assert.equal(log.split("native account tools passed").length - 1, 2, log);
  assert(!log.includes(token), "credential leaked into output");
  assert.deepEqual(requests, Array(4).fill("GET /api/reflect"));
  passed = true;
  console.log("native shared accounts, curl/Node TLS mediation and enforcement passed");
} finally {
  if (child && child.exitCode === null && child.signalCode === null) { child.kill("SIGTERM"); await closed; }
  await new Promise(resolve => server.close(resolve));
  if (passed) rmSync(root, { recursive: true, force: true });
  else console.error(`Retained account fixture at ${root}\n${log.replaceAll(token, "[REDACTED]")}`);
}
