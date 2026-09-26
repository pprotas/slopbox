import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { readFileSync } from "node:fs";

const [command, ...args] = process.argv.slice(2);
assert.ok(command, "usage: pi-rpc.mjs command [args...] < requests.jsonl");
const requests = readFileSync(0, "utf8").trim().split("\n").map(line => JSON.parse(line));
const pending = new Map();
for (const request of requests) {
  assert.ok(typeof request.id === "string" && request.id.length > 0, "RPC request needs an id");
  assert.ok(!pending.has(request.id), "RPC request ids must be unique");
  pending.set(request.id, request.type);
}

const child = spawn(command, args, { stdio: ["pipe", "pipe", "inherit"] });
const closed = new Promise(resolve => child.once("close", (code, signal) => resolve({ code, signal })));
let failure;
let escalation;
const stop = () => {
  child.kill("SIGTERM");
  escalation ??= setTimeout(() => child.kill("SIGKILL"), 1000);
};
child.on("error", error => { failure ??= error; });
child.stdin.on("error", error => { failure ??= error; stop(); });
const deadline = setTimeout(() => {
  failure ??= new Error(`Pi RPC timed out; pending: ${[...pending.keys()].join(", ") || "shutdown"}`);
  stop();
}, 30_000);

try {
  child.stdin.write(requests.map(request => JSON.stringify(request)).join("\n") + "\n");
  child.stdout.setEncoding("utf8");
  let buffered = "";
  for await (const chunk of child.stdout) {
    process.stdout.write(chunk);
    buffered += chunk;
    let newline;
    while ((newline = buffered.indexOf("\n")) !== -1) {
      const response = JSON.parse(buffered.slice(0, newline));
      buffered = buffered.slice(newline + 1);
      if (response.type !== "response") continue;
      assert.ok(pending.has(response.id), `unexpected RPC response id: ${response.id}`);
      assert.equal(response.command, pending.get(response.id), "RPC response command mismatch");
      assert.equal(response.success, true, `RPC command failed: ${JSON.stringify(response)}`);
      if (response.command === "bash") {
        assert.equal(response.data.exitCode, 0, "RPC bash command failed");
        assert.equal(response.data.cancelled, false, "RPC bash command was cancelled");
      }
      pending.delete(response.id);
      // EOF shuts Pi down, so keep stdin open until every response is complete.
      if (pending.size === 0) child.stdin.end();
    }
    assert.ok(buffered.length <= 1024 * 1024, "RPC response line is too large");
  }
  const { code, signal } = await closed;
  if (failure) throw failure;
  assert.equal(buffered, "", "incomplete RPC response");
  assert.equal(pending.size, 0, `Pi exited before responding to: ${[...pending.keys()].join(", ")}`);
  assert.equal(code, 0, `Pi exited with code ${code}, signal ${signal}`);
} catch (error) {
  console.error(error.message);
  process.exitCode = 1;
} finally {
  clearTimeout(deadline);
  if (child.exitCode === null && child.signalCode === null) stop();
  await closed;
  clearTimeout(escalation);
}
