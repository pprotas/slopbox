import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import test from "node:test";

const driver = fileURLToPath(new URL("./pi-rpc.mjs", import.meta.url));

test("waits past the old EOF deadline for every correlated response", () => {
  const response = {
    id: "bash",
    type: "response",
    command: "bash",
    success: true,
    data: { output: "snowman ☃\u2028text", exitCode: 0, cancelled: false },
  };
  const peer = `
    process.stdin.on("end", () => process.exit(0));
    process.stdin.once("data", () => {
      process.stdout.write(JSON.stringify({type:"bash_execution_update",id:"bash",delta:"not finished"}) + "\\n");
      process.stdout.write(JSON.stringify({id:"commands",type:"response",command:"get_commands",success:true}) + "\\n");
      setTimeout(() => {
        const bytes = Buffer.from(${JSON.stringify(JSON.stringify(response) + "\r\n")});
        const split = bytes.indexOf(Buffer.from("☃")) + 1;
        process.stdout.write(bytes.subarray(0, split));
        setTimeout(() => process.stdout.write(bytes.subarray(split)), 10);
      }, 2200);
    });
  `;
  const result = spawnSync(process.execPath, [driver, process.execPath, "-e", peer], {
    input: '{"id":"bash","type":"bash","command":"fixture"}\n{"id":"commands","type":"get_commands"}\n',
    encoding: "utf8",
    timeout: 10_000,
  });
  assert.equal(result.status, 0, result.stderr);
  const messages = result.stdout.trim().split("\n").map(line => JSON.parse(line));
  assert.deepEqual(messages.at(-1), response);
});

for (const [name, output, expected] of [
  ["early EOF", "", /Pi exited before responding/],
  ["incomplete JSONL", '{"id":"request"', /incomplete RPC response/],
  ["wrong id", '{"type":"response","id":"other"}\n', /unexpected RPC response id/],
  ["wrong command", '{"type":"response","id":"request","command":"get_state"}\n', /RPC response command mismatch/],
  ["failed command", '{"type":"response","id":"request","command":"bash","success":false}\n', /RPC command failed/],
  ["failed shell", '{"type":"response","id":"request","command":"bash","success":true,"data":{"exitCode":1,"cancelled":false}}\n', /RPC bash command failed/],
  ["cancelled shell", '{"type":"response","id":"request","command":"bash","success":true,"data":{"exitCode":0,"cancelled":true}}\n', /RPC bash command was cancelled/],
]) {
  test(`rejects ${name}`, () => {
    const peer = `process.stdin.once("data", () => process.stdout.write(${JSON.stringify(output)}, () => process.exit(0)));`;
    const result = spawnSync(process.execPath, [driver, process.execPath, "-e", peer], {
      input: '{"id":"request","type":"bash","command":"fixture"}\n',
      encoding: "utf8",
      timeout: 10_000,
    });
    assert.equal(result.status, 1, result.stderr);
    assert.match(result.stderr, expected);
  });
}

test("rejects a nonzero process exit after a valid response", () => {
  const peer = `
    process.stdin.on("end", () => process.exit(7));
    process.stdin.once("data", () => process.stdout.write('{"type":"response","id":"request","command":"get_commands","success":true}\\n'));
  `;
  const result = spawnSync(process.execPath, [driver, process.execPath, "-e", peer], {
    input: '{"id":"request","type":"get_commands"}\n',
    encoding: "utf8",
    timeout: 10_000,
  });
  assert.equal(result.status, 1, result.stderr);
  assert.match(result.stderr, /Pi exited with code 7/);
});

test("rejects duplicate request ids", () => {
  const result = spawnSync(process.execPath, [driver, process.execPath, "-e", "process.exit(0)"], {
    input: '{"id":"same","type":"get_commands"}\n{"id":"same","type":"get_commands"}\n',
    encoding: "utf8",
    timeout: 10_000,
  });
  assert.equal(result.status, 1, result.stderr);
  assert.match(result.stderr, /RPC request ids must be unique/);
});
