import { spawn } from "node:child_process";
import { createConnection } from "node:net";
import {
  type BashOperations,
  createBashTool,
  type ExtensionAPI,
} from "@earendil-works/pi-coding-agent";

const SLOPBOX = "/run/slopbox/slopbox";
const MODEL_PORT = process.env.SLOPBOX_MODEL_PROXY_PORT;
const SYNTHETIC_CODEX_JWT =
  "eyJhbGciOiJub25lIiwidHlwIjoiSldUIn0.eyJodHRwczovL2FwaS5vcGVuYWkuY29tL2F1dGgiOnsiY2hhdGdwdF9hY2NvdW50X2lkIjoic2xvcGJveCJ9fQ.";

function createSlopboxBashOperations(network: "none" | "general"): BashOperations {
  if (process.platform === "darwin") return createNativeBashOperations();
  return {
    exec(command, cwd, { onData, signal, timeout, env }) {
      if (signal?.aborted) return Promise.reject(new Error("aborted"));

      return new Promise((resolve, reject) => {
        const child = spawn(
          SLOPBOX,
          ["tool-run", "--network", network, "--", "/bin/sh", "-c", command],
          {
            cwd,
            env,
            detached: true,
            stdio: ["ignore", "pipe", "pipe"],
          },
        );

        let timedOut = false;
        const timeoutHandle =
          timeout && timeout > 0
            ? setTimeout(() => {
                timedOut = true;
                killProcessGroup(child.pid);
              }, timeout * 1000)
            : undefined;

        const onAbort = () => killProcessGroup(child.pid);
        signal?.addEventListener("abort", onAbort, { once: true });
        child.stdout?.on("data", onData);
        child.stderr?.on("data", onData);

        child.on("error", (error) => {
          if (timeoutHandle) clearTimeout(timeoutHandle);
          signal?.removeEventListener("abort", onAbort);
          reject(error);
        });
        child.on("close", (code) => {
          if (timeoutHandle) clearTimeout(timeoutHandle);
          signal?.removeEventListener("abort", onAbort);

          if (signal?.aborted) reject(new Error("aborted"));
          else if (timedOut) reject(new Error(`timeout:${timeout}`));
          else resolve({ exitCode: code ?? 1 });
        });
      });
    },
  };
}

function createNativeBashOperations(): BashOperations {
  const socket = process.env.SLOPBOX_NATIVE_SOCKET;
  const workspace = process.env.SLOPBOX_NATIVE_WORKSPACE;
  const bound = Number(process.env.SLOPBOX_NATIVE_TIMEOUT);
  if (!socket?.startsWith("/") || !workspace?.startsWith("/") || !Number.isInteger(bound) || bound < 1 || bound > 3605) {
    throw new Error("missing native Slopbox tool transport");
  }
  return {
    exec(command, cwd, { onData, signal, timeout }) {
      if (signal?.aborted) return Promise.reject(new Error("aborted"));
      const body = Buffer.from(`/bin/bash\0-c\0${command}\0`);
      if (cwd !== workspace || command.includes("\0") || body.length > 16384) {
        return Promise.reject(new Error("invalid native tool request"));
      }
      return new Promise((resolve, reject) => {
        const stream = createConnection({ path: socket });
        const timer = setTimeout(() => stream.destroy(new Error(`timeout:${timeout ?? bound}`)),
          Math.min(timeout && timeout > 0 ? timeout : bound, bound) * 1000);
        const abort = () => stream.destroy(new Error("aborted"));
        signal?.addEventListener("abort", abort, { once: true });
        if (signal?.aborted) abort();
        let bytes = Buffer.alloc(0);
        stream.on("connect", () => {
          const header = Buffer.alloc(4);
          header.writeUInt32BE(body.length);
          stream.write(Buffer.concat([header, body]));
        });
        stream.on("data", (chunk) => {
          if (bytes.length + chunk.length > 32768 + 36) {
            stream.destroy(new Error("native output limit exceeded"));
            return;
          }
          bytes = Buffer.concat([bytes, chunk]);
          if (bytes.length >= 4) {
            const size = bytes.readUInt32BE();
            if (size === 0 || size > 32768 + 32 || bytes.length > size + 4) {
              stream.destroy(new Error("invalid native response size"));
            }
          }
        });
        stream.on("error", reject);
        stream.on("end", () => {
          if (bytes.length < 4 || bytes.readUInt32BE() !== bytes.length - 4) {
            reject(new Error("incomplete native response"));
            return;
          }
          const separator = bytes.indexOf(10, 4);
          const status = bytes.subarray(4, separator).toString();
          if (separator < 5 || !/^(0|[1-9][0-9]{0,2})$/.test(status) || Number(status) > 255) {
            reject(new Error("invalid native exit status"));
            return;
          }
          onData(bytes.subarray(separator + 1));
          resolve({ exitCode: Number(status) });
        });
        stream.on("close", () => {
          clearTimeout(timer);
          signal?.removeEventListener("abort", abort);
          reject(new Error("native connection closed"));
        });
      });
    },
  };
}

function killProcessGroup(pid: number | undefined): void {
  if (!pid) return;
  try {
    process.kill(-pid, "SIGKILL");
  } catch {
    try {
      process.kill(pid, "SIGKILL");
    } catch {
      // The process has already exited.
    }
  }
}

export default function (pi: ExtensionAPI) {
  const denialsAtToolStart = new Map<string, Set<string>>();

  async function readDenials(
    signal: AbortSignal | undefined,
  ): Promise<string[]> {
    if (process.platform === "darwin") return [];
    try {
      const result = await pi.exec(SLOPBOX, ["denials"], {
        signal,
        timeout: 1000,
      });
      if (result.code !== 0) return [];
      return result.stdout
        .trim()
        .split("\n")
        .filter(Boolean);
    } catch {
      return [];
    }
  }

  pi.on("project_trust", () => ({ trusted: "no" }));
  pi.on("tool_execution_start", async (event, ctx) => {
    if (event.toolName !== "web_search" && event.toolName !== "web_fetch") return;
    const denials = await readDenials(ctx.signal);
    denialsAtToolStart.set(
      event.toolCallId,
      new Set(denials.map((line) => line.split(" ", 1)[0])),
    );
  });
  pi.on("tool_result", async (event, ctx) => {
    const baseline = denialsAtToolStart.get(event.toolCallId);
    denialsAtToolStart.delete(event.toolCallId);
    if (!event.isError) return;

    const output = event.content
      .filter((part) => part.type === "text")
      .map((part) => part.text)
      .join("\n");
    const explicitDenial =
      output.includes("NS_ERROR_PROXY_FORBIDDEN") ||
      output.includes("SLOPBOX_EGRESS_DENIED");
    if (!baseline && !explicitDenial) return;

    const denials = await readDenials(ctx.signal);
    let relevant = baseline
      ? denials.filter((line) => !baseline.has(line.split(" ", 1)[0]))
      : [];
    if (relevant.length === 0 && explicitDenial) {
      relevant = denials.slice(-10);
    }
    if (relevant.length === 0) return;

    const instructions = relevant.flatMap((line) => {
      const [requestId, method, destination] = line.split(" ", 4);
      return [
        `${method} ${destination}`,
        `  session: slopbox network approve ${requestId} --session`,
        `  project: slopbox network approve ${requestId} --project`,
      ];
    });
    return {
      content: [
        {
          type: "text" as const,
          text: [
            "Slopbox blocked network access.",
            "",
            ...instructions,
            "",
            "Run the appropriate approval command in a host terminal, then retry the tool.",
          ].join("\n"),
        },
      ],
    };
  });

  if (MODEL_PORT && process.env.OPENROUTER_API_KEY === "slopbox:openrouter") {
    pi.registerProvider("openrouter", {
      baseUrl: `http://127.0.0.1:${MODEL_PORT}/openrouter/api/v1`,
    });
  }
  if (MODEL_PORT && process.env.SLOPBOX_CODEX_ENABLED === "1") {
    pi.registerProvider("openai-codex", {
      baseUrl: `http://127.0.0.1:${MODEL_PORT}/openai-codex`,
      apiKey: SYNTHETIC_CODEX_JWT,
    });
  }

  const cwd = process.cwd();
  const network = process.env.SLOPBOX_PI_TOOL_NETWORK === "none" ? "none" : "general";
  const operations = createSlopboxBashOperations(network);
  const bash = createBashTool(cwd, {
    operations,
    exposeSessionEnvironment: false,
  });

  pi.registerTool({
    ...bash,
    label: `bash (Slopbox ${network})`,
  });

  pi.on("user_bash", () => ({ operations }));
}
