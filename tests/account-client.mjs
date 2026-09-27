import { readFileSync } from "node:fs";
import https from "node:https";

const [proxy, ca, destination] = process.argv.slice(2);
const agent = new https.Agent({
  proxyEnv: { HTTPS_PROXY: proxy, NO_PROXY: "" },
  ca: readFileSync(ca),
});
const request = https.get(destination, { agent, headers: { Authorization: "Bearer guest" } }, response => {
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
