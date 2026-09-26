import assert from "node:assert/strict";
import { existsSync, readFileSync, writeFileSync } from "node:fs";
import { createConnection } from "node:net";
import { setTimeout as delay } from "node:timers/promises";

const { id } = JSON.parse(readFileSync("fixture.json", "utf8"));
assert.equal(process.env.SLOPBOX_FIXTURE_NIX_ACTIVE, undefined);
assert.equal(process.env.IN_NIX_SHELL, undefined);
assert.equal(process.env.CARGO_HOME, undefined);
async function tool(args) {
  return await new Promise((resolve, reject) => {
    const body = Buffer.from(args.join("\0") + "\0");
    const header = Buffer.alloc(4);
    header.writeUInt32BE(body.length);
    const socket = createConnection(process.env.SLOPBOX_NATIVE_SOCKET, () => socket.write(Buffer.concat([header, body])));
    let bytes = Buffer.alloc(0);
    socket.setTimeout(125000, () => socket.destroy(new Error("tool timed out")));
    socket.on("error", reject);
    socket.on("data", chunk => {
      bytes = Buffer.concat([bytes, chunk]);
      if (bytes.length >= 4 && bytes.length === 4 + bytes.readUInt32BE()) {
        socket.end();
        const response = bytes.subarray(4).toString();
        try { assert(response.startsWith("0\n"), response); resolve(response.slice(2)); } catch (error) { reject(error); }
      }
    });
    socket.on("end", () => { if (bytes.length < 4 || bytes.length !== 4 + bytes.readUInt32BE()) reject(new Error("incomplete tool response")); });
  });
}
assert.match(await tool([process.execPath, "nix-probe.mjs", id]), /tools passed/);
const result = JSON.parse(readFileSync(`result-${id}.json`, "utf8"));
for (const path of [result.compiler, result.activation, result.cargoHome + "/nix-fixture-cache"]) {
  assert.throws(() => readFileSync(path), { code: "EPERM" }, path);
}
writeFileSync(`ready-${id}`, "ready");
while (!existsSync(`stop-${id}`)) await delay(25);
assert.match(await tool(["/bin/bash", "--noprofile", "--norc", "-c", "cargo --version && ./target/debug/native-nix-fixture"]), /Rust zlib /);
console.log(`native Nix ${id} session passed`);
