import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { existsSync, mkdirSync, readdirSync, readFileSync, writeFileSync } from 'node:fs';
import { setTimeout as delay } from 'node:timers/promises';

const [binary, root] = process.argv.slice(2);
const workspace = `${root}/clipboard-workspace`;
mkdirSync(workspace);
const png = 'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jRZkAAAAASUVORK5CYII=';
writeFileSync(`${workspace}/probe.cjs`, `
const fs = require('node:fs');
const assert = require('node:assert/strict');
assert.equal(process.env.WAYLAND_DISPLAY, undefined);
assert.equal(process.env.DISPLAY, undefined);
const image = '/run/slopbox-clipboard/image-fixture.png';
assert.equal(fs.readFileSync(image).toString('base64'), '${png}');
assert.throws(() => fs.writeFileSync(image, 'changed'), { code: 'EROFS' });
assert.throws(() => fs.writeFileSync('/run/slopbox-clipboard/request', 'paste'), { code: 'EROFS' });
process.stdout.write('\\x16\\x1b[118;5u');
console.log('CLIPBOARD_READ_ONLY');
`);
const pidFile = `${root}/clipboard.pid`;
const command = 'stty rows 32 cols 160; before=$(stty -g); "$SLOPBOX" run --workspace "$CLIP_WORKSPACE" --dev-env none -- pi --offline </dev/tty & pid=$!; printf "%s" "$pid" >"$PID_FILE"; wait "$pid"; result=$?; test "$(stty -g)" = "$before" || exit 90; exit "$result"';
const child = spawn('script', ['-q', '-e', '-c', command, '/dev/null'], {
  env: { ...process.env, SLOPBOX: binary, CLIP_WORKSPACE: workspace, PID_FILE: pidFile, WAYLAND_DISPLAY: 'slopbox-e2e-no-display' },
  stdio: ['pipe', 'pipe', 'pipe'],
});
let output = '';
let pending = '';
let ended = false;
let supervisor;
const closed = new Promise(resolve => child.on('close', code => { ended = true; resolve(code); }));
for (const stream of [child.stdout, child.stderr]) stream.on('data', bytes => { output += bytes; pending += bytes; });
const deadline = setTimeout(() => child.kill('SIGKILL'), 20000);
async function waitFor(pattern) {
  const until = Date.now() + 5000;
  while (Date.now() < until) {
    const match = pending.match(pattern);
    if (match) { pending = pending.slice(match.index + match[0].length); return; }
    if (ended) break;
    await delay(10);
  }
  throw new Error(`timed out waiting for ${pattern}\n${output}`);
}
let directory;
try {
  await waitFor(/\x1b\[\?2004h/);
  supervisor = Number(readFileSync(pidFile, 'utf8'));
  await delay(300);
  child.stdin.write('\x1b[200~pasted \x16\x1b[118;5u\x1b[201~');
  await delay(200);
  assert(!output.includes('SLOPBOX CLIPBOARD'), 'pasted control bytes triggered a clipboard read');
  child.stdin.write('\x03');
  await delay(100);
  child.stdin.write('\x1b[118;5u');
  await waitFor(/SLOPBOX CLIPBOARD/);
  await waitFor(/Press Enter or Escape/);
  child.stdin.write('\r');
  await waitFor(/\x1b\[\?2026h/);
  await delay(300);

  // No compositor in CI: capture is unit-tested with a bounded helper fixture.
  // Seed the host-owned import directory to verify the real Pi/inner-tool mount.
  const boxes = `${process.env.XDG_DATA_HOME}/slopbox/boxes`;
  const imports = readdirSync(boxes).flatMap(box => readdirSync(`${boxes}/${box}`)
    .filter(name => name.startsWith('run-'))
    .map(name => `${boxes}/${box}/${name}/clipboard`)).filter(existsSync);
  assert.equal(imports.length, 1);
  [directory] = imports;
  assert.equal(readdirSync(directory).length, 0);
  writeFileSync(`${directory}/image-fixture.png`, Buffer.from(png, 'base64'), { mode: 0o600 });
  const notices = output.split('SLOPBOX CLIPBOARD').length;
  child.stdin.write('!!node ./probe.cjs\r');
  await waitFor(/CLIPBOARD_READ_ONLY/);
  assert.equal(output.split('SLOPBOX CLIPBOARD').length, notices, 'guest output triggered a clipboard read');
  child.stdin.write('\x1b[200~ /run/slopbox-clipboard/image-fixture.png \x1b[201~');
  await waitFor(/\/run\/slopbox-clipboard\/image-fixture\.png/);
  assert.equal(readFileSync(`${directory}/image-fixture.png`).toString('base64'), png);
  process.kill(supervisor, 'SIGTERM');
  assert.equal(await closed, 143);
  assert(!existsSync(directory), 'session imports survived normal shutdown');
  console.log('e2e: clipboard errors restore Pi; imported images are read-only and session-local');
} finally {
  clearTimeout(deadline);
  if (!ended) {
    if (supervisor) { try { process.kill(supervisor, 'SIGTERM'); } catch {} }
    child.kill('SIGTERM');
    await Promise.race([closed, delay(1000)]);
    if (!ended) child.kill('SIGKILL');
  }
}
