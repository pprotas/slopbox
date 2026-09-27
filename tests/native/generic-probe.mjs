import assert from "node:assert/strict";
import { chmodSync, existsSync, readFileSync, writeFileSync } from "node:fs";
import { createConnection } from "node:net";

const fixture = JSON.parse(readFileSync("fixture.json", "utf8"));
for (const path of fixture.hidden) {
  assert.throws(() => readFileSync(path), { code: "EPERM" }, `host path readable: ${path}`);
}
for (const name of ["FIXTURE_ACCOUNT_TOKEN", "SSH_AUTH_SOCK", "SLOPBOX_NATIVE_SOCKET", "SLOPBOX_TEST_TLS_CA"]) {
  assert(!process.env[name], `host/control environment exposed: ${name}`);
}
assert.equal(process.env.ANTHROPIC_AUTH_TOKEN, "slopbox:fixture");
assert(existsSync(fixture.executable));
assert.throws(() => chmodSync(fixture.executable, 0o755), { code: "EPERM" });

function connect(host, port) {
  return new Promise((resolve, reject) => {
    const socket = createConnection({ host, port });
    socket.setTimeout(2000, () => socket.destroy(new Error("connection timed out")));
    socket.once("error", reject);
    socket.once("connect", () => { socket.destroy(); resolve(); });
  });
}
for (const [host, port] of [["1.1.1.1", 80], ["127.0.0.1", fixture.upstreamPort]]) {
  await assert.rejects(connect(host, port), { code: "EPERM" });
}
assert(!Object.values(process.env).includes("disposable-model-authority-canary"));
// Exercise the broker relay without making an upstream model request.
const model = await fetch(`http://127.0.0.1:${process.env.SLOPBOX_MODEL_PROXY_PORT}/openrouter/api/v1/chat/completions`);
assert.equal(model.status, 405);
assert.equal(await model.text(), "method not allowed\n");
const response = await fetch(`${process.env.SLOPBOX_AUTHENTICATED_HTTP_BASE_URL}/fixture/v1/messages`, {
  method: "POST", headers: { "Content-Type": "application/json", "anthropic-version": "2023-06-01" },
  body: JSON.stringify({ model: "fixture-account-probe", messages: [] }),
});
assert.equal(response.status, 200);
assert.equal((await response.json()).content[0].text, "account-accessible");
writeFileSync("outer-passed", "passed");
console.log("probe-complete");
