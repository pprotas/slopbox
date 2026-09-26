import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { existsSync, readFileSync, readdirSync, realpathSync, writeFileSync } from "node:fs";
import { createConnection } from "node:net";
import { join } from "node:path";

const spec = JSON.parse(readFileSync("fixture.json", "utf8"));
const id = process.argv[2];
assert.equal(process.env.SLOPBOX_FIXTURE_NIX_ACTIVE, "1");
assert.equal(process.env.IN_NIX_SHELL, "impure");
for (const key of ["SLOPBOX_NATIVE_SOCKET", "SLOPBOX_MODEL_PROXY_PORT", "OPENROUTER_API_KEY", "SLOPBOX_HOST_CANARY", "SSH_AUTH_SOCK", "NIX_REMOTE", "BASH_ENV"]) {
  assert.equal(process.env[key], undefined, key);
}
for (const path of [spec.outside, spec.unrelated, spec.hostConfig]) {
  assert.throws(() => readFileSync(path), { code: "EPERM" }, path);
}
for (const path of ["/nix/store", "/nix/var/nix/profiles"]) {
  assert.throws(() => readdirSync(path), { code: "EPERM" }, path);
}
await new Promise((resolve, reject) => {
  const socket = createConnection("/nix/var/nix/daemon-socket/socket");
  socket.on("connect", () => { socket.destroy(); reject(new Error("Nix daemon exposed")); });
  socket.on("error", error => {
    try { assert.equal(error.code, "EPERM"); resolve(); } catch (error) { reject(error); }
  });
});
function run(program, args) {
  const result = spawnSync(program, args, { encoding: "utf8", timeout: 60000, maxBuffer: 16384 });
  assert.ifError(result.error);
  assert.equal(result.status, 0, `${program}: ${result.stdout}\n${result.stderr}`);
  return result.stdout;
}
function selected(program) {
  const path = program.startsWith("/") ? program : process.env.PATH.split(":").map(path => join(path, program)).find(existsSync);
  assert(path, `${program} missing`);
  const canonical = realpathSync(path);
  assert(canonical.startsWith("/nix/store/"), `${program} came from ${canonical}`);
  return canonical;
}
const compiler = selected(process.env.CC);
selected(process.env.CXX);
selected("cargo");
selected("rustc");
selected("bash");
assert(process.env.SDKROOT.startsWith("/nix/store/"), process.env.SDKROOT);
assert.throws(() => writeFileSync(compiler, "bad"), error => ["EPERM", "EACCES", "EROFS"].includes(error.code));
const activation = readFileSync("activation-path", "utf8").trim();
assert.throws(() => writeFileSync(activation, "bad"), { code: "EPERM" });
const cache = join(process.env.CARGO_HOME, "nix-fixture-cache");
if (id === "first") {
  assert(!existsSync(cache));
  writeFileSync(cache, "private-cache-reused");
} else {
  assert.equal(readFileSync(cache, "utf8"), "private-cache-reused");
}
run(process.env.CC, ["hello.c", "-lz", "-o", "hello-c"]);
assert.match(run("./hello-c", []), /^C zlib /);
run(process.env.CXX, ["hello.cc", "-std=c++17", "-lz", "-o", "hello-cxx"]);
assert.match(run("./hello-cxx", []), /^C\+\+ zlib /);
run("cargo", ["build", "--offline", "--quiet"]);
assert.match(run("./target/debug/native-nix-fixture", []), /^Rust zlib /);
writeFileSync(`result-${id}.json`, JSON.stringify({ compiler, activation, cargoHome: process.env.CARGO_HOME }));
console.log(`native Nix ${id} tools passed`);
