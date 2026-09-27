import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import http from "node:http";
import https from "node:https";
import tls from "node:tls";

const [proxy, ca, destination] = process.argv.slice(2);
const url = new URL(destination);
const tunnel = http.request(proxy, {
  method: "CONNECT",
  path: url.host.includes(":") ? url.host : `${url.host}:443`,
  headers: { Host: url.host },
});
tunnel.setTimeout(5000, () => tunnel.destroy(new Error("CONNECT timeout")));
tunnel.on("error", error => { console.error(error.message); process.exitCode = 1; });
tunnel.on("connect", (response, socket, head) => {
  assert.equal(response.statusCode, 200);
  assert.equal(head.length, 0);
  const agent = new https.Agent();
  agent.createConnection = () => tls.connect({
    socket, servername: url.hostname, ca: readFileSync(ca),
  });
  const request = https.get(url, { agent, headers: { Authorization: "Bearer guest" } }, response => {
    process.stdout.write(`status=${response.statusCode}\n`);
    response.pipe(process.stdout);
    response.on("end", () => agent.destroy());
  });
  request.setTimeout(5000, () => request.destroy(new Error("HTTPS timeout")));
  request.on("error", error => {
    console.error(error.message);
    agent.destroy();
    process.exitCode = 1;
  });
});
tunnel.end();
