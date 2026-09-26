import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";

const fixture = JSON.parse(readFileSync("github-fixture.json", "utf8"));
assert.equal(process.env.GH_TOKEN, "slopbox-brokered-authentication");
assert.equal(process.env.GH_ENTERPRISE_TOKEN, "slopbox-brokered-authentication");
assert.equal(process.env.SLOPBOX_NATIVE_SOCKET, undefined);
assert.equal(process.env.SSH_AUTH_SOCK, undefined);
assert.equal(process.env.GITHUB_HOST_CANARY, undefined);
assert.throws(() => readFileSync(fixture.credentials), error => error.code === "EPERM");
assert.throws(() => writeFileSync(join(process.env.GH_CONFIG_DIR, "config.yml"), ""), error => error.code === "EPERM");

function gh(args, success = true) {
  const result = spawnSync(fixture.gh, args, { encoding: "utf8", timeout: 10000 });
  assert.ifError(result.error);
  assert.equal(result.status === 0, success, result.stderr);
  return result.stdout.trim();
}
assert.equal(gh(["api", "--hostname", "github.native.invalid", "user", "--jq", ".login"]), "fixture");
assert.deepEqual(JSON.parse(gh(["repo", "view", "github.native.invalid/owner/repository", "--json", "nameWithOwner"])), { nameWithOwner: "owner/repository" });
assert.equal(gh(["api", "--hostname", "github.native.invalid", "reflect", "--jq", ".token"]), "[REDACTED]");
gh(["api", "--hostname", "github.native.invalid", "--method", "DELETE", "repos/owner/repository"], false);
gh(["api", "https://other.native.invalid/api/v3/user"], false);
console.log("native gh tool checks passed");
