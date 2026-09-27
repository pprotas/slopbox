import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { readFileSync, writeFileSync } from "node:fs";
import { connect } from "node:net";

const fixture = JSON.parse(readFileSync("accounts-fixture.json", "utf8"));
const ca = process.env.SLOPBOX_ACCOUNT_CA;
const proxy = process.env.SLOPBOX_ACCOUNT_PROXY;
assert(ca && proxy);
assert.match(readFileSync(ca, "utf8"), /BEGIN CERTIFICATE/);
assert.throws(() => writeFileSync(ca, "changed"), error => error.code === "EPERM");
assert.throws(() => readFileSync(fixture.config), error => error.code === "EPERM");
for (const name of ["SLOPBOX_NATIVE_SOCKET", "SLOPBOX_MODEL_PROXY_PORT", "ACCOUNT_FIXTURE_TOKEN", "SLOPBOX_TEST_ACCOUNT", "SSH_AUTH_SOCK", "HTTPS_PROXY", "GH_TOKEN"]) {
  assert.equal(process.env[name], undefined, name);
}
function curl(args, success = true) {
  const result = spawnSync("/usr/bin/curl", ["--silent", "--show-error", "--fail", "--max-time", "5", "--noproxy", "", "--proxy", proxy, ...args], { encoding: "utf8", timeout: 10000 });
  assert.ifError(result.error);
  assert.equal(result.status === 0, success, result.stderr);
  return result.stdout;
}
assert.equal(curl(["--cacert", ca, "https://forgejo.native.invalid/api/reflect"]), "[REDACTED]");
const node = spawnSync(process.execPath, ["account-client.mjs", proxy, ca, "https://forgejo.native.invalid/api/reflect"], { encoding: "utf8", timeout: 10000 });
assert.ifError(node.error);
assert.equal(node.status, 0, node.stderr);
assert.equal(node.stdout, "status=200\n[REDACTED]");
curl(["https://forgejo.native.invalid/api/reflect"], false);
curl(["--cacert", ca, "https://other.native.invalid/api/reflect"], false);
curl(["--cacert", ca, "https://forgejo.native.invalid/outside"], false);
curl(["--cacert", ca, "--request", "DELETE", "https://forgejo.native.invalid/api/reflect"], false);
await new Promise((resolve, reject) => {
  const socket = connect({ host: "127.0.0.1", port: fixture.upstreamPort });
  socket.on("connect", () => { socket.destroy(); reject(new Error("direct upstream connection escaped sandbox")); });
  socket.on("error", error => { assert.equal(error.code, "EPERM"); resolve(); });
  socket.setTimeout(3000, () => { socket.destroy(); reject(new Error("direct connection did not fail promptly")); });
});
console.log("native account tools passed");
