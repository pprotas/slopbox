import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { existsSync, mkdtempSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { join } from "node:path";

const directory = mkdtempSync(join(process.cwd(), "homebrew-check-"));
try {
  function run(name, args) {
    const executable = process.env.PATH.split(":")
      .map((path) => join(path, name)).find(existsSync);
    assert(executable, `${name} is missing from the tool PATH`);
    const resolved = realpathSync(executable);
    assert(["/opt/homebrew/Cellar/", "/usr/local/Cellar/"].some((prefix) => resolved.startsWith(prefix)),
      `${name} did not select a Homebrew installation: ${resolved}`);
    const result = spawnSync(executable, args, {
      cwd: directory, encoding: "utf8", timeout: 10000, maxBuffer: 16384,
    });
    assert.ifError(result.error);
    assert.equal(result.status, 0, `${name} failed (${result.signal}): ${result.stdout}\n${result.stderr}`);
    return result.stdout;
  }
  assert.match(run("git", ["--version"]), /^git version /);
  run("git", ["-c", "init.defaultBranch=main", "init", "-q"]);
  writeFileSync(join(directory, "example.txt"), "homebrew-dependency-loaded\n");
  assert.match(run("git", ["status", "--short"]), /\?\? example\.txt/);
  assert.equal(run("rg", ["--fixed-strings", "homebrew-dependency-loaded", "example.txt"]),
    "homebrew-dependency-loaded\n");
  console.log("Homebrew Git and ripgrep passed");
} finally {
  rmSync(directory, { recursive: true, force: true });
}
