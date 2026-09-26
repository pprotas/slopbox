import assert from "node:assert/strict";
import { connect } from "node:net";
import { join } from "node:path";

assert.equal(process.env.GH_TOKEN, undefined, "tool authentication marker leaked to Pi");
assert.equal(process.env.GH_CONFIG_DIR, undefined, "tool gh configuration leaked to Pi");
const workspace = process.env.SLOPBOX_NATIVE_WORKSPACE;
const socket = connect(process.env.SLOPBOX_NATIVE_SOCKET);
const body = Buffer.from([process.execPath, join(workspace, "github-probe.mjs"), ""].join("\0"));
const frame = Buffer.alloc(4);
frame.writeUInt32BE(body.length);
let output = Buffer.alloc(0);
socket.on("data", chunk => { output = Buffer.concat([output, chunk]); });
socket.on("error", error => { throw error; });
socket.on("end", () => {
  assert.equal(output.readUInt32BE(0), output.length - 4);
  const response = output.subarray(4).toString();
  assert(response.startsWith("0\n"), response);
  assert(response.includes("native gh tool checks passed"), response);
  console.log("native gh harness/tool separation passed");
});
// EOF cancels tool execution; keep the write side open while awaiting the reply.
socket.on("connect", () => { socket.write(Buffer.concat([frame, body])); });
