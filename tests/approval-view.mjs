import assert from 'node:assert/strict';
import { spawn, execFileSync } from 'node:child_process';
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { setTimeout as delay } from 'node:timers/promises';

const [binary, root] = process.argv.slice(2);
const workspace = `${root}/approval-view-workspace`;
mkdirSync(workspace);
writeFileSync(`${workspace}/guest.sh`, `set -eu
stty -echo
printf '\\035s 1\\np 1\\nGUEST_FORGED_OUTPUT\\n'
curl -sS --max-time 10 http://view.slopbox-e2e.invalid/ >initial
(while test ! -e inject-diagnostic; do sleep 0.02; done
node -e 'const p=new URL(process.env.HTTP_PROXY); const s=require("net").connect(Number(p.port),p.hostname,()=>s.end("BROKEN\\r\\n\\r\\n")); s.on("data",()=>{}); s.on("error",()=>{});'
touch diagnostic-done) &
printf 'GUEST_READY\\n'
IFS= read -r reply
printf 'GUEST_INPUT:%s\\n' "$reply"
test "$reply" = resume
curl -sS --max-time 10 http://view.slopbox-e2e.invalid/ >after-approval
slopbox denials >denials
printf 'GUEST_RETRIED\\n'
IFS= read -r reply
test "$reply" = finish
curl -sS --max-time 10 http://view.slopbox-e2e.invalid/ >after-revoke
printf 'GUEST_DONE\\n'
`);
const command = `stty rows 32 cols 120; before=$(stty -g); "$SLOPBOX" run --workspace "$VIEW_WORKSPACE" --approval-view --dev-env none -- /bin/sh "$VIEW_WORKSPACE/guest.sh"; result=$?; test "$(stty -g)" = "$before" || exit 90; printf '\\nHOST_TERMINAL_RESTORED:%s\\n' "$result"; exit "$result"`;
const child = spawn('script', ['-q', '-e', '-c', command, '/dev/null'], {
  env: { ...process.env, SLOPBOX: binary, VIEW_WORKSPACE: workspace },
  stdio: ['pipe', 'pipe', 'pipe'],
});
let output = '';
let pending = '';
let ended = false;
const closed = new Promise((resolve) => child.on('close', code => { ended = true; resolve(code); }));
for (const stream of [child.stdout, child.stderr]) stream.on('data', bytes => { output += bytes; pending += bytes; });
const deadline = setTimeout(() => { child.kill('SIGKILL'); }, 30000);
async function waitFor(pattern) {
  const until = Date.now() + 10000;
  while (Date.now() < until) {
    const match = pending.match(pattern);
    if (match) { pending = pending.slice(match.index + match[0].length); return match; }
    if (ended) break;
    await delay(10);
  }
  throw new Error(`Timed out waiting for ${pattern}\n${output}`);
}
function send(text) { child.stdin.write(text); }
try {
  await waitFor(/GUEST_READY/);
  assert(!output.includes('SLOPBOX HOST APPROVALS'), 'guest output opened the host view');
  assert.match(readFileSync(`${workspace}/initial`, 'utf8'), /no matching allow rule/);
  const status = execFileSync(binary, ['status', '--workspace', workspace, '--verbose'], { encoding: 'utf8' });
  const state = status.match(/^project-state: (.+)$/m)[1];
  const rules = () => {
    try { return JSON.parse(readFileSync(`${state}/network-rules.json`)).rules; }
    catch (error) { if (error.code === 'ENOENT') return []; throw error; }
  };
  send('\x1dp 1\nyes\n');
  await waitFor(/SLOPBOX HOST APPROVALS/);
  await delay(100);
  assert.equal(rules().length, 0, 'queued input approved a rule');
  writeFileSync(`${workspace}/inject-diagnostic`, '');
  for (let attempt = 0; attempt < 100; attempt++) {
    try { readFileSync(`${workspace}/diagnostic-done`); break; }
    catch (error) { if (error.code !== 'ENOENT' || attempt === 99) throw error; }
    await delay(20);
  }
  assert(!output.includes('slopbox general gateway:'), 'broker diagnostics reached the host view');
  send('s 1\n');
  const cancelled = (await waitFor(/Type ([a-f0-9]{12}) and Enter to confirm/))[1];
  send('yes\n');
  await waitFor(/confirmation cancelled/);
  assert.equal(rules().length, 0);
  send('s 1\n');
  const code = (await waitFor(/Type ([a-f0-9]{12}) and Enter to confirm/))[1];
  assert.notEqual(code, cancelled);
  send(`${code}\n`);
  await waitFor(/Approved rule-[a-f0-9]+; retry the operation/);
  assert.equal(rules().length, 1);
  assert.equal(rules()[0].scope, 'session');
  send('q\n');
  await delay(300);
  send('resume\n');
  await waitFor(/GUEST_INPUT:resume/);
  await waitFor(/GUEST_RETRIED/);
  assert(!readFileSync(`${workspace}/after-approval`, 'utf8').includes('no matching allow rule'));
  assert.match(readFileSync(`${workspace}/denials`, 'utf8'), /DNS resolution failed/);
  // Exercise the enhanced keyboard encoding as well as the classic control byte.
  send('\x1b[93;5u');
  await waitFor(/SLOPBOX HOST APPROVALS/);
  const index = (await waitFor(/(\d+): RULE view\.slopbox-e2e\.invalid:80 session/))[1];
  send(`r ${index}\n`);
  const revoke = (await waitFor(/Type ([a-f0-9]{12}) and Enter to confirm/))[1];
  send(`${revoke}\n`);
  await waitFor(/Revoked rule-/);
  assert(rules()[0].revoked_at_ms);
  send('q\n');
  await delay(300);
  send('finish\n');
  await waitFor(/GUEST_DONE/);
  await waitFor(/HOST_TERMINAL_RESTORED:0/);
  assert.equal(await closed, 0);
  assert.match(output, /recent broker diagnostics/);
  assert.match(output, /slopbox general gateway:/);
  assert.match(readFileSync(`${workspace}/after-revoke`, 'utf8'), /no matching allow rule/);
  console.log('e2e: host approval view isolates input, confirms scope, revokes, and restores the terminal');
} finally {
  clearTimeout(deadline);
  if (!ended) child.kill('SIGKILL');
}
