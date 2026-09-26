import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { once } from "node:events";
import { mkdtempSync, mkdirSync, readFileSync, readdirSync, rmSync, writeFileSync, existsSync, symlinkSync } from "node:fs";
import { createServer } from "node:http";
import { join } from "node:path";
import { setTimeout as delay } from "node:timers/promises";

const [driver, slopbox, pi, harness = "trusted", workspaceMode = "live", smoke] = process.argv.slice(2);
const readOnly = workspaceMode === "read-only";
const root = mkdtempSync("/private/var/tmp/slopbox-cli-");
const quote = (value) => `'${value.replaceAll("'", "'\\''")}'`;
for (const name of ["workspace", "home", "config/slopbox", "data", "outside"]) mkdirSync(join(root, name), { recursive: true });
writeFileSync(join(root, "outside/canary"), "disposable-host-canary");
writeFileSync(join(root, "config/slopbox/config.toml"), `[policy]\nharness=${JSON.stringify(harness)}\nworkspace=${JSON.stringify(workspaceMode)}\n[macos]\nnode=${JSON.stringify(process.execPath)}\npi_cli=${JSON.stringify(pi)}\n`);
mkdirSync(join(root, "workspace/.pi/extensions"), { recursive: true });
writeFileSync(join(root, "workspace/.pi/extensions/evil.ts"), "throw new Error('project extension executed');\n");
writeFileSync(join(root, "workspace/.pi/settings.json"), '{"defaultProjectTrust":"always"}');
if (smoke === "homebrew") {
  writeFileSync(join(root, "workspace/homebrew.mjs"), readFileSync(new URL("./homebrew.mjs", import.meta.url)));
}

const hostAgent = join(root, "home/.pi/agent");
for (const name of ["extensions", "skills/fixture", "prompts", "npm/node_modules/fixture-package", "npm/node_modules/fixture-helper"]) {
  mkdirSync(join(hostAgent, name), { recursive: true });
}
const hostAuth = join(hostAgent, "auth.json");
writeFileSync(hostAuth, '{"fixture":{"type":"api_key","key":"host-auth-canary"}}');
writeFileSync(join(hostAgent, "AGENTS.md"), "fixture-global-instructions");
writeFileSync(join(hostAgent, "settings.json"), JSON.stringify({
  defaultProvider: "openrouter", quietStartup: true, apiKey: "host-settings-canary",
  packages: ["npm:fixture-package"], retry: { enabled: false },
}));
writeFileSync(join(hostAgent, "skills/fixture/SKILL.md"), "---\nname: fixture\ndescription: fixture skill\n---\nUse the fixture.");
const hostPrompt = join(hostAgent, "prompts/fixture.md");
writeFileSync(hostPrompt, "Read-only fixture prompt.");
symlinkSync(hostAuth, join(root, "workspace/credential-link"));
symlinkSync(hostAuth, join(hostAgent, "extensions/credential-link"));
const fileContent = "original\n" + "x".repeat(20000) + "\n";
writeFileSync(join(root, "workspace/file-tools.txt"), fileContent);
writeFileSync(join(hostAgent, "extensions/fixture.ts"), `
import assert from "node:assert/strict";
import { readFileSync, writeFileSync } from "node:fs";
import { Type } from "typebox";
export default function (pi) {
  pi.registerTool({ name: "fixture_plugin", label: "Fixture", description: "Trusted host fixture",
    parameters: Type.Object({}), async execute() {
      for (const path of [${JSON.stringify(hostAuth)}, ${JSON.stringify(join(hostAgent, "extensions/credential-link"))}]) {
        assert.throws(() => readFileSync(path), { code: "EPERM" });
      }
      assert.throws(() => writeFileSync(${JSON.stringify(hostPrompt)}, "bad"), { code: "EPERM" });
      const settings = JSON.parse(readFileSync(process.env.PI_CODING_AGENT_DIR + "/settings.json", "utf8"));
      assert.equal(settings.quietStartup, true);
      assert.equal(settings.apiKey, undefined);
      assert.deepEqual(JSON.parse(readFileSync(process.env.PI_CODING_AGENT_DIR + "/auth.json", "utf8")), {});
      return { content: [{ type: "text", text: "trusted-plugin-passed" }], details: {} };
    }
  });
}
`);
writeFileSync(join(hostAgent, "npm/node_modules/fixture-helper/package.json"), '{"main":"index.js"}');
writeFileSync(join(hostAgent, "npm/node_modules/fixture-helper/index.js"), 'module.exports = "npm-dependency-passed";');
writeFileSync(join(hostAgent, "npm/node_modules/fixture-package/package.json"), '{"pi":{"extensions":["main.ts"]}}');
writeFileSync(join(hostAgent, "npm/node_modules/fixture-package/main.ts"), `
import marker from "fixture-helper";
import { Type } from "typebox";
export default function (pi) {
  pi.registerTool({ name: "fixture_package", label: "Package", description: "Installed npm fixture",
    parameters: Type.Object({}), async execute() {
      return { content: [{ type: "text", text: marker }], details: {} };
    }
  });
}
`);

let modelPort;
let requests = 0;
let brokerError;
let settings;
const check = (marker) => `${quote(process.execPath)} check.mjs ${modelPort} ${quote(settings)} ${marker}`;
writeFileSync(join(root, "workspace/check.mjs"), `
import assert from 'node:assert/strict';
import { readFileSync, writeFileSync } from 'node:fs';
import { createConnection } from 'node:net';
const [port, settings, marker] = process.argv.slice(2);
assert.equal(process.cwd(), ${JSON.stringify(join(root, "workspace"))});
for (const name of ['OPENROUTER_API_KEY', 'SLOPBOX_MODEL_PROXY_PORT', 'SLOPBOX_NATIVE_SOCKET', 'SLOPBOX_TEST_OPENROUTER']) assert.equal(process.env[name], undefined, name);
for (const path of [${JSON.stringify(join(root, "outside/canary"))}, settings]) {
  assert.throws(() => readFileSync(path), { code: 'EPERM' });
}
await new Promise((resolve, reject) => {
  const stream = createConnection({ host: '127.0.0.1', port: Number(port) });
  stream.on('connect', () => { stream.destroy(); reject(new Error('tool reached model broker')); });
  stream.on('error', (error) => { try { assert.equal(error.code, 'EPERM'); resolve(); } catch (error) { reject(error); } });
});
if (${readOnly}) assert.throws(() => writeFileSync(marker, 'sandboxed'), { code: 'EPERM' });
else writeFileSync(marker, 'sandboxed');
console.log(marker);
`);
const denied = (result) => {
  assert.match(result.content, /EPERM|EACCES/);
  assert(!result.content.includes("host-auth-canary"));
};
const calls = [
  ["write", () => ({ path: "file-tools.txt", content: fileContent }), readOnly ? denied : (result) => assert.match(result.content, /Successfully wrote/)],
  ["edit", () => ({ path: "file-tools.txt", edits: [{ oldText: "original", newText: "edited" }] }), readOnly ? denied : (result) => assert.match(result.content, /Successfully/)],
  ["read", () => ({ path: "file-tools.txt", limit: 1 }), (result) => assert.match(result.content, readOnly ? /original/ : /edited/)],
  ["read", () => ({ path: hostAuth }), denied],
  ["read", () => ({ path: "credential-link" }), denied],
  ["write", () => ({ path: "credential-link", content: "bad" }), denied],
  ["write", () => ({ path: join(root, "outside/canary"), content: "bad" }), denied],
  ["write", () => ({ path: hostPrompt, content: "bad" }), denied],
  ...(harness === "trusted" ? [
    ["fixture_plugin", () => ({}), (result) => assert.match(result.content, /trusted-plugin-passed/)],
    ["fixture_package", () => ({}), (result) => assert.match(result.content, /npm-dependency-passed/)],
  ] : []),
  ["bash", () => ({ command: check("model-tool-passed") }), (result) => assert.match(result.content, /model-tool-passed/)],
];
const broker = createServer(async (request, response) => {
  try {
    assert.equal(request.url, "/api/v1/chat/completions");
    assert.equal(request.headers.authorization, "Bearer disposable-host-provider-key");
    let body = "";
    for await (const chunk of request) { body += chunk; assert(body.length < 1024 * 1024); }
    const payload = JSON.parse(body);
    assert(++requests <= calls.length + 1, "unexpected model replay");
    let delta;
    let finish;
    if (requests === 1) {
      const expected = ["read", "write", "edit", "bash", ...(harness === "trusted" ? ["fixture_plugin", "fixture_package"] : [])];
      assert.deepEqual(payload.tools.map((tool) => tool.function.name).sort(), expected.sort());
      assert.equal(JSON.stringify(payload.messages).includes("fixture-global-instructions"), harness !== "none");
    } else {
      const result = payload.messages.findLast((message) => message.role === "tool");
      assert(result, JSON.stringify(payload.messages));
      assert(!result.content.includes("disposable-host-provider-key"));
      calls[requests - 2][2](result);
    }
    if (requests <= calls.length) {
      const [name, parameters] = calls[requests - 1];
      delta = { role: "assistant", tool_calls: [{ index: 0, id: `native-call-${requests}`, type: "function", function: { name, arguments: JSON.stringify(parameters()) } }] };
      finish = "tool_calls";
    } else {
      delta = { role: "assistant", content: "native CLI complete" };
      finish = "stop";
    }
    response.writeHead(200, { "content-type": "text/event-stream" });
    response.end(`data: ${JSON.stringify({ id: "native", object: "chat.completion.chunk", model: "openai/gpt-4o", choices: [{ index: 0, delta, finish_reason: finish }], usage: { prompt_tokens: 10, completion_tokens: 10, total_tokens: 20 } })}\n\ndata: [DONE]\n\n`);
  } catch (error) {
    brokerError = error;
    response.writeHead(500).end("fixture failed");
  }
});
broker.listen(0, "127.0.0.1");
await once(broker, "listening");
const child = spawn(driver, ["--exact", "native_cli_tests::native_cli_fixture", "--ignored", "--nocapture"], {
  cwd: join(root, "workspace"),
  env: {
    HOME: join(root, "home"), XDG_CONFIG_HOME: join(root, "config"), XDG_DATA_HOME: join(root, "data"), PATH: "/usr/bin:/bin",
    OPENROUTER_API_KEY: "disposable-host-provider-key",
    SLOPBOX_TEST_SLOPBOX: slopbox, SLOPBOX_TEST_OPENROUTER: `127.0.0.1:${broker.address().port}`,
  },
  stdio: ["pipe", "pipe", "pipe"],
});
const closed = once(child, "close");
let stderr = "";
let text = "";
const events = [];
child.stderr.setEncoding("utf8").on("data", (chunk) => { stderr += chunk; });
child.stdout.setEncoding("utf8").on("data", (chunk) => {
  text += chunk;
  assert(text.length < 1024 * 1024);
  let index;
  while ((index = text.indexOf("\n")) !== -1) {
    const line = text.slice(0, index);
    text = text.slice(index + 1);
    if (line.startsWith("{")) events.push(JSON.parse(line));
  }
});
const deadline = Date.now() + 30000;
async function waitFor(predicate) {
  while (true) {
    const index = events.findIndex(predicate);
    if (index !== -1) return events.splice(index, 1)[0];
    assert.ifError(brokerError);
    assert(child.exitCode === null && child.signalCode === null, stderr);
    assert(Date.now() < deadline, `native CLI timeout: ${stderr}\n${JSON.stringify(events)}`);
    await delay(5);
  }
}
let sequence = 0;
function send(type, fields = {}) {
  const id = String(++sequence);
  child.stdin.write(`${JSON.stringify({ id, type, ...fields })}\n`);
  return id;
}
async function response(id) {
  const event = await waitFor((event) => event.type === "response" && event.id === id);
  assert(event.success, JSON.stringify(event));
  return event.data;
}
try {
  const state = await response(send("get_state"));
  const url = new URL(state.model.baseUrl);
  assert.equal(url.hostname, "127.0.0.1");
  assert.equal(url.pathname, "/openrouter/api/v1");
  modelPort = Number(url.port);
  const boxes = join(root, "data/slopbox/boxes");
  const box = join(boxes, readdirSync(boxes)[0]);
  const run = readdirSync(box).find((name) => name.startsWith("run-"));
  settings = join(box, run, "pi-state/settings.json");
  assert(existsSync(settings));
  if (smoke === "homebrew") {
    const result = await response(send("bash", { command: `${quote(process.execPath)} homebrew.mjs` }));
    assert.equal(result.exitCode, 0, result.output);
    assert(result.output.includes("Homebrew Git and ripgrep passed"), result.output);
    console.log("Homebrew Git and ripgrep passed through the production supervisor");
  }
  // Exceed the relay's buffers so the gateway must wait for more request data.
  await response(send("prompt", { message: `Exercise the native CLI. ${"x".repeat(128 * 1024)}` }));
  await waitFor((event) => event.type === "agent_settled");
  const last = await response(send("get_last_assistant_text"));
  const errors = events.filter((event) => event.type === "message_end").map((event) => event.message?.errorMessage).filter(Boolean);
  assert.equal(last.text, "native CLI complete", JSON.stringify({ requests, errors, stderr, last }));
  const bash = await response(send("bash", { command: check("user-bash-passed") }));
  assert.equal(bash.exitCode, 0, bash.output);
  const general = await response(send("bash", { command: "/usr/bin/curl -sS --max-time 3 --proxy \"$HTTP_PROXY\" http://example.invalid/" }));
  assert(general.output.includes("SLOPBOX_EGRESS_DENIED"), general.output);
  if (!readOnly) {
    const pidFile = join(root, "workspace/cancel-pid");
    const pending = send("bash", { command: `(/bin/sleep 30) & echo $! > ${quote(pidFile)}; wait` });
    while (!existsSync(pidFile)) { assert(Date.now() < deadline); await delay(5); }
    await response(send("abort_bash"));
    assert((await response(pending)).cancelled);
    const pid = Number(readFileSync(pidFile, "utf8"));
    while (true) {
      try { process.kill(pid, 0); } catch (error) { assert.equal(error.code, "ESRCH"); break; }
      assert(Date.now() < deadline, "cancelled descendant survived");
      await delay(5);
    }
  }
  child.stdin.end();
  assert.equal((await closed)[0], 0, stderr);
  assert.ifError(brokerError);
  assert.equal(requests, calls.length + 1);
  assert.equal(readFileSync(join(root, "outside/canary"), "utf8"), "disposable-host-canary");
  assert.equal(readFileSync(hostAuth, "utf8"), '{"fixture":{"type":"api_key","key":"host-auth-canary"}}');
  assert.equal(readFileSync(hostPrompt, "utf8"), "Read-only fixture prompt.");
  assert.equal(readFileSync(join(root, "workspace/file-tools.txt"), "utf8"), readOnly ? fileContent : fileContent.replace("original", "edited"));
  assert(!existsSync(join(box, run)), "session configuration was not cleaned up");
  assert(readdirSync(join(box, "home/.pi/agent/sessions")).some((name) => name.endsWith(".jsonl")));
  console.log(`native CLI model/tool round trip passed (${harness}, ${workspaceMode})`);
} finally {
  if (child.exitCode === null && child.signalCode === null) { child.kill("SIGTERM"); await closed; }
  broker.closeAllConnections();
  broker.close();
  rmSync(root, { recursive: true, force: true });
}
