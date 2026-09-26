import { createConnection } from "node:net";
import {
  type BashOperations,
  createBashTool,
  type ExtensionAPI,
} from "@earendil-works/pi-coding-agent";

export default function (pi: ExtensionAPI) {
  const socket = process.env.SLOPBOX_PROBE_SOCKET;
  const port = Number(process.env.SLOPBOX_PROBE_MODEL_PORT);
  if (!socket?.startsWith("/") || !Number.isInteger(port) || port < 1 || port > 65535) {
    throw new Error("missing native probe configuration");
  }
  const workspace = process.cwd();
  const operations: BashOperations = {
    exec(command, cwd, { onData, signal, timeout }) {
      if (signal?.aborted) return Promise.reject(new Error("aborted"));
      const body = Buffer.from(`/bin/sh\0-c\0${command}\0`);
      if (cwd !== workspace || command.includes("\0") || body.length > 16384) {
        return Promise.reject(new Error("invalid native tool request"));
      }
      return new Promise((resolve, reject) => {
        const stream = createConnection({ path: socket });
        const timer = setTimeout(
          () => stream.destroy(new Error(`timeout:${timeout ?? 5}`)),
          Math.min(timeout && timeout > 0 ? timeout * 1000 : 5000, 5000),
        );
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

  pi.on("project_trust", () => ({ trusted: "no" }));
  pi.registerTool({
    ...createBashTool(workspace, { operations, exposeSessionEnvironment: false }),
    label: "bash (native Seatbelt probe)",
  });
  pi.on("user_bash", () => ({ operations }));
  pi.registerCommand("probe-quit", { handler: async (_args, ctx) => ctx.shutdown() });
  pi.registerProvider("seatbelt-probe", {
    baseUrl: `http://127.0.0.1:${port}/v1`,
    apiKey: "synthetic-model-marker",
    api: "openai-completions",
    models: [{
      id: "fake", name: "Fake broker", reasoning: false, input: ["text"],
      cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
      contextWindow: 128000, maxTokens: 4096,
    }],
  });
}
