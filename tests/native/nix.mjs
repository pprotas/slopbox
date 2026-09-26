import assert from "node:assert/strict";
import { spawn, spawnSync } from "node:child_process";
import { once } from "node:events";
import { existsSync, mkdtempSync, mkdirSync, readFileSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { setTimeout as delay } from "node:timers/promises";

const [slopbox, nix] = process.argv.slice(2);
assert(slopbox?.startsWith("/") && nix?.startsWith("/"), "absolute Slopbox and Nix paths required");
const root = mkdtempSync("/private/var/tmp/slopbox-nix-");
const workspace = join(root, "workspace");
const children = [];
let passed = false;
const environment = {
  HOME: join(root, "home"), XDG_CONFIG_HOME: join(root, "config"), XDG_DATA_HOME: join(root, "data"),
  PATH: `${dirname(nix)}:/usr/bin:/bin`, SLOPBOX_HOST_CANARY: "must-not-enter-project-environment",
};
function nixCommand(args) {
  const result = spawnSync(nix, ["--extra-experimental-features", "nix-command flakes", ...args], {
    env: environment, encoding: "utf8", timeout: 120000,
  });
  assert.ifError(result.error);
  assert.equal(result.status, 0, result.stderr);
  return result.stdout.trim();
}
async function start(id, spec) {
  writeFileSync(join(workspace, "fixture.json"), JSON.stringify({ ...spec, id }));
  const child = spawn(slopbox, ["run", "--dev-env", "flake", "--", "pi"], { cwd: workspace, env: environment });
  const state = { child, output: "", closed: false };
  children.push(state);
  child.stdout.on("data", bytes => { state.output += bytes; });
  child.stderr.on("data", bytes => { state.output += bytes; });
  state.exit = once(child, "close").then(
    ([code, signal]) => { state.closed = true; return [code, signal]; },
    error => { state.closed = true; state.output += String(error); return [null, "spawn-error"]; },
  );
  const deadline = Date.now() + 600000;
  while (!existsSync(join(workspace, `ready-${id}`))) {
    assert(!state.closed, state.output);
    assert(Date.now() < deadline, `native Nix startup timed out\n${state.output}`);
    await delay(50);
  }
  state.result = JSON.parse(readFileSync(join(workspace, `result-${id}.json`), "utf8"));
  for (const binary of ["hello-c", "hello-cxx", "target/debug/native-nix-fixture"]) {
    const result = spawnSync("/usr/bin/otool", ["-L", join(workspace, binary)], {
      env: { PATH: "/usr/bin:/bin" }, encoding: "utf8", timeout: 10000,
    });
    assert.ifError(result.error);
    assert.equal(result.status, 0, result.stderr);
    assert.match(result.stdout, /\/nix\/store\/[^\s]+\/lib\/libz\./);
  }
  state.profile = join(dirname(state.result.activation), "dev-profile");
  assert(realpathSync(state.profile).startsWith("/nix/store/"));
  return state;
}
async function stop(state, id) {
  writeFileSync(join(workspace, `stop-${id}`), "stop");
  const timer = setTimeout(() => state.child.kill("SIGTERM"), 150000);
  const [code, signal] = await state.exit;
  clearTimeout(timer);
  assert.equal(signal, null, state.output);
  assert.equal(code, 0, state.output);
  assert.match(state.output, /session passed/);
  assert(!existsSync(state.profile), "session GC root survived normal cleanup");
  assert(!existsSync(state.result.activation), "activation survived normal cleanup");
}
try {
  for (const path of ["workspace/src", "home", "config/slopbox", "data", "outside", "pi/dist"]) mkdirSync(join(root, path), { recursive: true });
  const outside = join(root, "outside/canary");
  const hostConfig = join(root, "home/.gitconfig");
  writeFileSync(outside, "host-canary");
  writeFileSync(hostConfig, "host-config-canary");
  writeFileSync(join(root, "pi/package.json"), '{"name":"@earendil-works/pi-coding-agent","type":"module"}');
  writeFileSync(join(root, "pi/dist/cli.js"), readFileSync(new URL("./nix-harness.mjs", import.meta.url)));
  writeFileSync(join(workspace, "nix-probe.mjs"), readFileSync(new URL("./nix-probe.mjs", import.meta.url)));
  writeFileSync(join(workspace, "flake.nix"), readFileSync(new URL("./nix/flake.nix", import.meta.url)));
  writeFileSync(join(workspace, "flake.lock"), readFileSync(new URL("../../flake.lock", import.meta.url)));
  writeFileSync(join(workspace, "Cargo.toml"), '[package]\nname="native-nix-fixture"\nversion="0.1.0"\nedition="2024"\n');
  writeFileSync(join(workspace, "src/main.rs"), '#[link(name="z")] unsafe extern "C" { fn zlibVersion() -> *const std::ffi::c_char; }\nfn main() { println!("Rust zlib {}", unsafe { std::ffi::CStr::from_ptr(zlibVersion()) }.to_str().unwrap()); }\n');
  writeFileSync(join(workspace, "hello.c"), '#include <stdio.h>\n#include <zlib.h>\nint main(void) { printf("C zlib %s\\n", zlibVersion()); return 0; }\n');
  writeFileSync(join(workspace, "hello.cc"), '#include <iostream>\n#include <string>\n#include <zlib.h>\nint main() { std::cout << "C++ zlib " << std::string(zlibVersion()) << std::endl; }\n');
  writeFileSync(join(root, "config/slopbox/config.toml"), `[policy]\nruntime="project"\nharness="none"\nnetwork="none"\ncredentials="none"\n[macos]\nnode=${JSON.stringify(process.execPath)}\npi_cli=${JSON.stringify(join(root, "pi/dist/cli.js"))}\ntool_timeout_seconds=120\n`);
  writeFileSync(join(root, "unrelated-data"), `unrelated fixture ${root}`);
  const unrelated = nixCommand(["store", "add-file", join(root, "unrelated-data")]);
  nixCommand(["build", "--out-link", join(root, "unrelated-root"), unrelated]);
  const first = await start("first", { outside, unrelated, hostConfig });
  const second = await start("second", { outside, unrelated, hostConfig });
  assert.notEqual(first.profile, second.profile);
  assert.equal(first.result.cargoHome, second.result.cargoHome);
  await stop(first, "first");
  assert(existsSync(second.profile), "one session unpinned another's closure");
  assert(existsSync(second.result.activation));
  await stop(second, "second");
  const config = join(root, "config/slopbox/config.toml");
  writeFileSync(config, readFileSync(config, "utf8").replace('runtime="project"', 'runtime="host"'));
  const host = await start("host", { outside, unrelated, hostConfig });
  await stop(host, "host");
  assert.equal(readFileSync(unrelated, "utf8"), `unrelated fixture ${root}`);
  assert.equal(readFileSync(outside, "utf8"), "host-canary");
  assert.equal(readFileSync(hostConfig, "utf8"), "host-config-canary");
  assert.equal(readFileSync(join(workspace, "hook-calls"), "utf8"), "hook\nhook\nhook\nhook\nhook\nhook\n");
  passed = true;
  console.log("native Nix builds, role separation, private cache and closure lifetimes passed");
} finally {
  for (const state of children) {
    if (!state.closed) { state.child.kill("SIGTERM"); await state.exit; }
    if (!passed) process.stderr.write(state.output);
  }
  if (passed) rmSync(root, { recursive: true, force: true });
  else console.error(`native Nix fixture retained at ${root}`);
}
