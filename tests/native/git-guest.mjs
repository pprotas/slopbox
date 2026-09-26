import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { chmodSync, readFileSync, writeFileSync } from "node:fs";
import { createConnection } from "node:net";

const spec = JSON.parse(readFileSync("fixture.json", "utf8"));
const phase = process.argv[2];
function git(args, success = true, error) {
  const result = spawnSync("git", args, { encoding: "utf8", timeout: 15000 });
  assert.ifError(result.error);
  if (success) assert.equal(result.status, 0, result.stderr);
  else assert.notEqual(result.status, 0, "unexpected Git success");
  if (error) assert.match(result.stderr, error);
  return result.stdout.trim();
}

for (const name of ["SSH_AUTH_SOCK", "NATIVE_FIXTURE_FORGEJO_TOKEN", "SLOPBOX_TEST_ACCOUNT", "HTTP_PROXY", "SLOPBOX_MODEL_PROXY_PORT"]) {
  assert.equal(process.env[name], undefined, name);
}
for (const path of [spec.key, spec.hostConfig]) {
  assert.throws(() => readFileSync(path), { code: "EPERM" });
}
await new Promise((resolve, reject) => {
  const stream = createConnection(spec.agentSocket);
  stream.on("connect", () => { stream.destroy(); reject(new Error("raw SSH agent exposed")); });
  stream.on("error", (error) => {
    try { assert.equal(error.code, "EPERM"); resolve(); } catch (error) { reject(error); }
  });
});
const config = process.env.GIT_CONFIG_GLOBAL;
assert.notEqual(config, "/dev/null");
assert.equal(process.env.GIT_CONFIG_NOSYSTEM, "1");
assert.throws(() => writeFileSync(config, "bad"), { code: "EPERM" });
const base = process.env.SLOPBOX_AUTHENTICATED_HTTP_BASE_URL;
assert.equal(git(["remote", "get-url", "origin"]), `${base}/git`);
assert.equal(git(["remote", "get-url", "--push", "origin"]), `${base}/git`);
assert.equal(git(["config", "--local", "--get", "remote.origin.url"]), spec.remote);
const other = "ssh://git@forgejo.native.invalid/org/other.git";
assert.equal(git(["ls-remote", "--get-url", other]), other);
git(["fetch", "origin"]);
git(["pull", "--ff-only"]);

if (phase === "signing") {
  assert.equal(git(["rev-parse", "HEAD"]), spec.upstream);
  assert.equal(git(["config", "--get", "user.name"]), "Native fixture");
  assert.equal(git(["config", "--get", "user.email"]), "native@example.invalid");
  assert.equal(git(["config", "--get", "commit.gpgSign"]), "true");
  const helper = git(["config", "--get", "gpg.ssh.program"]);
  const wrapper = readFileSync(helper, "utf8");
  const socket = wrapper.match(/--socket '([^']+)' --/)[1];
  const executor = wrapper.match(/exec '([^']+)' __git-sign/)[1];
  assert.throws(() => writeFileSync(helper, "bad"), { code: "EPERM" });
  assert.throws(() => chmodSync(executor, 0o700), { code: "EPERM" });
  git(["-c", "user.email=other@example.invalid", "commit", "--allow-empty", "-m", "wrong identity"], false, /Git signing broker refused the request/);
  git(["tag", "-s", "must-not-exist", "-m", "not a commit"], false, /Git signing broker refused the request/);
  writeFileSync("message", "native signed commit\n\n" + "x".repeat(128 * 1024) + "\n");
  git(["commit", "--allow-empty", "-F", "message"]);
  git(["push", "origin", "HEAD:refs/heads/native-signed"]);
  writeFileSync("signing-state.json", JSON.stringify({ helper, socket, config, commit: git(["rev-parse", "HEAD"]) }));
} else {
  assert.equal(phase, "routes-only");
  git(["config", "--get", "commit.gpgSign"], false);
  git(["config", "--get", "gpg.ssh.program"], false);
  git(["config", "--get", "user.signingKey"], false);
  git(["config", "--get", "user.name"], false);
}

let response = await fetch(`${base}/api/v1/user`, { headers: { authorization: "token guest-placeholder" } });
assert.equal(response.status, 200);
assert.equal((await response.json()).login, "native-fixture");
response = await fetch(`${base}/api/v1/reflect`);
assert.deepEqual(await response.json(), { token: "[REDACTED]" });
response = await fetch(`${base}/api/v1/user`, { method: "DELETE" });
assert.equal(response.status, 405);
await assert.rejects(fetch(`${base}/unconfigured/v1/user`), /fetch failed/);
await assert.rejects(fetch(`${base}/api/../unconfigured/v1/user`), /fetch failed/);
if (phase === "signing") {
  response = await fetch(`${base}/api/v1/repos/org/fixture/pulls`, {
    method: "POST", headers: { "content-type": "application/json" },
    body: JSON.stringify({ base: "main", head: "native-signed", title: "disposable PR" }),
  });
  assert.equal(response.status, 201);
  assert.equal((await response.json()).number, 1);
}
console.log(`native Git ${phase} passed`);
