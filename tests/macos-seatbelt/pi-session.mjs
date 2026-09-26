import assert from "node:assert/strict";
import { execFile, spawn } from "node:child_process";
import { once } from "node:events";
import { existsSync, mkdirSync, readFileSync, readdirSync, writeFileSync } from "node:fs";
import { createServer } from "node:http";
import { createServer as createNetServer } from "node:net";
import { join } from "node:path";
import { setTimeout as delay } from "node:timers/promises";
import { promisify } from "node:util";

const [mode, root, probe, piCli, worker, account, modelPort] = process.argv.slice(2);
const runtime = join(root, "pi-runtime");
const workspace = join(root, "workspace");
const quote = (value) => `'${value.replaceAll("'", "'\\''")}'`;
const check = (operation, target) => `${quote(probe)} ${operation} ${quote(target)}`;

function checks(port, marker, accountRoute = account) {
  return [
    "set -e",
    check("write", join(workspace, marker)),
    check("cwd", workspace),
    check("env-missing", "SLOPBOX_PROBE_MODEL_PORT"),
    check("env-missing", "SLOPBOX_PROBE_SOCKET"),
    check("tcp-reply", accountRoute),
    ...[
      ["read", join(root, "harness/canary")],
      ["read", join(root, "control/canary")],
      ["read", join(root, "outside/canary")],
      ["write", join(runtime, "pi-extension.ts")],
      ["tcp", `127.0.0.1:${port}`],
      ["unix", join(root, "control/native/tool.sock")],
    ].map(([operation, target]) => `if ${check(operation, target)}; then exit 90; else test "$?" -eq 77; fi`),
    `printf '%s\\n' ${quote(marker)}`,
  ].join("; ");
}

async function host() {
  const requests = [];
  let brokerError;
  const broker = createServer(async (request, response) => {
    try {
      let body = "";
      for await (const chunk of request) {
        body += chunk;
        assert(body.length < 1024 * 1024);
      }
      assert.equal(request.method, "POST");
      assert.equal(request.url, "/v1/chat/completions");
      assert.equal(request.headers.authorization, "Bearer synthetic-model-marker");
      const payload = JSON.parse(body);
      requests.push(payload);
      assert(requests.length <= 2, "unexpected model retry");
      let delta;
      let finish;
      if (requests.length === 1) {
        assert(payload.tools.some((tool) => tool.function.name === "bash"));
        delta = { role: "assistant", tool_calls: [{
          index: 0, id: "probe-call", type: "function",
          function: { name: "bash", arguments: JSON.stringify({
            command: (() => {
              const [model, account] = readFileSync(join(root, "control/native-ready"), "utf8").trim().split("\n");
              return checks(model, "model-tool-passed", `127.0.0.1:${account}`);
            })(),
          }) },
        }] };
        finish = "tool_calls";
      } else {
        const result = payload.messages.findLast((message) => message.role === "tool");
        assert(result?.content.includes("model-tool-passed"), JSON.stringify(result));
        assert(result.content.includes("account\nallowed"), JSON.stringify(result));
        assert.equal((result.content.match(/probe errno=Some\(1\)/g) ?? []).length, 6);
        delta = { role: "assistant", content: "native session complete" };
        finish = "stop";
      }
      response.writeHead(200, { "content-type": "text/event-stream" });
      response.end(`data: ${JSON.stringify({
        id: "probe", object: "chat.completion.chunk", model: "fake",
        choices: [{ index: 0, delta, finish_reason: finish }],
        usage: { prompt_tokens: 100, completion_tokens: 10, total_tokens: 110 },
      })}\n\ndata: [DONE]\n\n`);
    } catch (error) {
      brokerError = error;
      response.writeHead(500).end("fake broker assertion failed");
    }
  });
  const accountBroker = createNetServer({ allowHalfOpen: true }, (stream) => {
    stream.setTimeout(1000, () => stream.destroy());
    let body = "";
    stream.setEncoding("utf8").on("data", (chunk) => {
      body += chunk;
      if (body.length > 4) stream.destroy();
    });
    stream.on("error", (error) => { brokerError = error; });
    stream.on("end", () => {
      if (body === "ping") stream.end("account\n");
      else stream.destroy();
    });
  });
  broker.listen(join(root, "control/model.sock"));
  accountBroker.listen(join(root, "control/account.sock"));
  await Promise.all([once(broker, "listening"), once(accountBroker, "listening")]);
  try {
    const crash = mode === "host-crash";
    const execution = promisify(execFile)(probe, [
      crash ? "run-session-crash" : "run-session", root, worker, process.execPath, piCli,
    ], { env: {}, cwd: "/", timeout: 35000, maxBuffer: 32768 });
    if (crash) {
      const failed = assert.rejects(execution, (error) => error.signal === "SIGKILL");
      const deadline = Date.now() + 25000;
      while (!existsSync(join(workspace, "cancel-pid"))) {
        assert(execution.child.exitCode === null && execution.child.signalCode === null, "coordinator exited early");
        assert(Date.now() < deadline, "Pi did not reach crash checkpoint");
        await delay(5);
      }
      assert(execution.child.kill("SIGKILL"));
      await failed;
    } else {
      const { stdout } = await execution;
      assert(stdout.includes("Pi RPC session passed"), stdout);
    }
    assert.ifError(brokerError);
    assert.equal(requests.length, 2);
    assert(!existsSync(join(workspace, "project-extension-ran")));
    assert.equal(readFileSync(join(root, "outside/canary"), "utf8"), "disposable canary");
    for (const marker of ["model-tool-passed", "user-bash-passed"]) {
      assert.equal(readFileSync(join(workspace, marker), "utf8"), "changed by probe");
    }
    assert(readdirSync(join(root, "harness/agent/sessions")).some((name) => name.endsWith(".jsonl")));
    assert.equal(existsSync(join(root, "control/native")), crash, "unexpected native session ownership state");
    console.log(`native Pi model round-trip, leased brokers, tool isolation, user_bash and ${crash ? "coordinator SIGKILL" : "cancellation"} passed`);
  } finally {
    broker.closeAllConnections();
    broker.close();
    accountBroker.close();
  }
}

async function guest() {
  const agent = join(root, "harness/agent");
  mkdirSync(join(agent, "sessions"), { recursive: true });
  writeFileSync(join(agent, "settings.json"), JSON.stringify({
    defaultProjectTrust: "never", retry: { enabled: false, provider: { maxRetries: 0 } },
    compaction: { enabled: false }, transport: "sse", enableInstallTelemetry: false,
  }));
  writeFileSync(join(agent, "auth.json"), "{}\n");
  const child = spawn(process.execPath, [
    piCli, "--mode", "rpc", "--offline", "--no-extensions", "--no-skills",
    "--no-prompt-templates", "--no-themes", "--no-context-files", "--no-approve",
    "--no-builtin-tools", "--tools", "bash", "-e", join(runtime, "pi-extension.ts"),
    "--provider", "seatbelt-probe", "--model", "fake", "--thinking", "off",
    "--session-dir", join(agent, "sessions"),
  ], {
    cwd: workspace,
    env: {
      HOME: join(root, "harness"), TMPDIR: join(root, "harness"), PATH: "/usr/bin:/bin",
      PI_CODING_AGENT_DIR: agent, PI_OFFLINE: "1", PI_TELEMETRY: "0",
      SLOPBOX_PROBE_SOCKET: join(root, "control/native/tool.sock"),
      SLOPBOX_PROBE_MODEL_PORT: modelPort,
    },
    stdio: ["pipe", "pipe", "pipe"],
  });
  const closed = once(child, "close");
  const events = [];
  let text = "";
  let stderr = "";
  child.stderr.setEncoding("utf8").on("data", (chunk) => { stderr += chunk; });
  child.stdout.setEncoding("utf8").on("data", (chunk) => {
    text += chunk;
    assert(text.length < 1024 * 1024);
    let newline;
    while ((newline = text.indexOf("\n")) !== -1) {
      const line = text.slice(0, newline);
      text = text.slice(newline + 1);
      if (line.trim()) events.push(JSON.parse(line));
    }
  });
  const deadline = Date.now() + 20000;
  async function waitFor(predicate) {
    while (true) {
      const index = events.findIndex(predicate);
      if (index !== -1) return events.splice(index, 1)[0];
      assert(child.exitCode === null && child.signalCode === null, stderr);
      assert(Date.now() < deadline, `RPC timeout: ${stderr}\n${JSON.stringify(events)}`);
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
    assert.equal(state.model.provider, "seatbelt-probe");
    await response(send("prompt", { message: "Exercise the native tool boundary." }));
    await waitFor((event) => event.type === "agent_settled");
    const result = await response(send("get_last_assistant_text"));
    assert.equal(result.text, "native session complete", JSON.stringify(events));
    const bash = await response(send("bash", { command: checks(modelPort, "user-bash-passed") }));
    assert.equal(bash.exitCode, 0, bash.output);
    assert(bash.output.includes("user-bash-passed"));
    assert.equal((bash.output.match(/probe errno=Some\(1\)/g) ?? []).length, 6);
    const pid = join(workspace, "cancel-pid");
    const pending = send("bash", { command: check("detached-wait", pid) });
    while (!existsSync(pid)) {
      assert(Date.now() < deadline, "tool did not start");
      await delay(5);
    }
    if (mode === "guest-crash") {
      await response(pending);
      throw new Error("coordinator did not kill the running Pi session");
    }
    await response(send("abort_bash"));
    assert((await response(pending)).cancelled);
    assert(!events.some((event) => event.type === "extension_error"), JSON.stringify(events));
    send("prompt", { message: "/probe-quit" });
    assert.equal((await closed)[0], 0, stderr);
    console.log("Pi RPC session passed");
  } finally {
    child.stdin.end();
  }
}

await (mode.startsWith("host") ? host() : guest());
