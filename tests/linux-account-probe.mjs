import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { existsSync, readFileSync, writeFileSync } from "node:fs";
import { connect } from "node:net";

const fixture = JSON.parse(readFileSync("account-fixture.json", "utf8"));
for (const name of ["ACCOUNT_FIXTURE_TOKEN", "SSH_AUTH_SOCK", "SSL_CERT_FILE", "OPENROUTER_API_KEY", "SLOPBOX_MODEL_PROXY_PORT", "HTTP_PROXY", "HTTPS_PROXY", "GH_TOKEN"]) {
  assert.equal(process.env[name], undefined, name);
}
for (const path of [fixture.config, fixture.key, fixture.agent, "/run/slopbox-host/model/gateway.sock", "/run/slopbox-host/general/gateway.sock"]) {
  assert.equal(existsSync(path), false, path);
}
if (fixture.disabled) {
  assert.equal(process.env.SLOPBOX_ACCOUNT_CA, undefined);
  assert.equal(process.env.SLOPBOX_ACCOUNT_PROXY, undefined);
  assert.equal(existsSync("/run/slopbox-host/authenticated-http/gateway.sock"), false);
  assert.equal(existsSync("/run/slopbox/git-sign"), false);
  assert.equal(existsSync("/run/slopbox-host/git-signing/gateway.sock"), false);
  console.log("disabled account/signing checks passed");
  process.exit(0);
}
const ca = process.env.SLOPBOX_ACCOUNT_CA;
const proxy = process.env.SLOPBOX_ACCOUNT_PROXY;
assert(ca && proxy);
assert.match(readFileSync(ca, "utf8"), /BEGIN CERTIFICATE/);
assert.throws(() => writeFileSync(ca, "changed"), error => ["EROFS", "EACCES"].includes(error.code));
function curl(args, success = true) {
  const result = spawnSync("curl", ["--silent", "--show-error", "--fail", "--max-time", "5", "--noproxy", "", "--proxy", proxy, ...args], { encoding: "utf8", timeout: 10000 });
  assert.ifError(result.error);
  assert.equal(result.status === 0, success, result.stderr);
  return result.stdout;
}
assert.equal(curl(["--cacert", ca, `${fixture.origin}/api/reflect`]), "[REDACTED]");
const node = spawnSync(process.execPath, ["account-client.mjs", proxy, ca, `${fixture.origin}/api/reflect`], { encoding: "utf8", timeout: 10000 });
assert.ifError(node.error);
assert.equal(node.status, 0, node.stderr);
assert.equal(node.stdout, "status=200\n[REDACTED]");
assert.equal(curl(["--noproxy", "*", `${process.env.SLOPBOX_AUTHENTICATED_HTTP_BASE_URL}/forge/reflect`]), "[REDACTED]");
curl([`${fixture.origin}/api/reflect`], false);
curl(["--cacert", ca, "https://other.invalid/api/reflect"], false);
curl(["--cacert", ca, `${fixture.origin}/outside`], false);
curl(["--cacert", ca, "--request", "DELETE", `${fixture.origin}/api/reflect`], false);
curl(["--cacert", ca, "--header", "Host: other.invalid", `${fixture.origin}/api/reflect`], false);
await new Promise((resolve, reject) => {
  const socket = connect({ host: "127.0.0.1", port: new URL(fixture.origin).port });
  socket.on("connect", () => { socket.destroy(); reject(new Error("direct upstream connection escaped sandbox")); });
  socket.on("error", error => { assert(["ECONNREFUSED", "ENETUNREACH", "EACCES"].includes(error.code), error.message); resolve(); });
  socket.setTimeout(3000, () => { socket.destroy(); reject(new Error("direct connection did not fail promptly")); });
});
const git = spawnSync("git", ["commit", "--quiet", "--allow-empty", "-m", "shared identity fixture"], { encoding: "utf8", timeout: 10000 });
assert.ifError(git.error);
assert.equal(git.status, 0, git.stderr);
console.log("shared account and signing tools passed");
