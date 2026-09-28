import assert from "node:assert/strict";
import { connect } from "node:net";
import { join } from "node:path";

assert.equal(process.env.SLOPBOX_ACCOUNT_CA, undefined);
assert.equal(process.env.SLOPBOX_ACCOUNT_PROXY, undefined);
assert.equal(process.env.ACCOUNT_FIXTURE_TOKEN, undefined);
const socket = connect(process.env.SLOPBOX_NATIVE_SOCKET);
const body = Buffer.from([process.execPath, join(process.env.SLOPBOX_NATIVE_WORKSPACE, "accounts-probe.mjs"), ""].join("\0"));
const frame = Buffer.alloc(4);
frame.writeUInt32BE(body.length);
let output = Buffer.alloc(0);
socket.on("data", chunk => { output = Buffer.concat([output, chunk]); });
socket.on("error", error => { throw error; });
socket.on("end", () => {
  assert.equal(output.readUInt32BE(0), output.length - 4);
  const response = output.subarray(4).toString();
  assert(response.startsWith("0\n"), response);
  assert(response.includes("native account tools passed"), response);
  console.log("native account separation passed");
});
socket.on("connect", () => socket.write(Buffer.concat([frame, body])));
