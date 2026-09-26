import assert from 'node:assert/strict';
import { spawn, execFileSync } from 'node:child_process';
import { readFileSync, writeFileSync } from 'node:fs';
import { setTimeout as delay } from 'node:timers/promises';

const [binary, root] = process.argv.slice(2);
const workspace = `${root}/approval-view-workspace`;
writeFileSync(`${workspace}/lifecycle.sh`, `set -eu
stty -echo
printf 'LIFECYCLE_READY\\n'
IFS= read -r reply
test "$reply" = finish
printf 'LIFECYCLE_DONE\\n'
`);
for (const scenario of ['suspend', 'signal', 'pi']) {
  const pidFile = `${root}/${scenario}.pid`;
  const modeFile = `${root}/${scenario}.termios`;
  const invocation = '"$SLOPBOX" run --workspace "$VIEW_WORKSPACE" --approval-view --dev-env none -- /bin/sh "$VIEW_WORKSPACE/lifecycle.sh"';
  const command = `stty rows 32 cols 120; before=$(stty -g); printf '%s' "$before" >"$MODE_FILE"; tty >"$MODE_FILE.tty"; ${scenario === 'pi' ? 'cd "$VIEW_WORKSPACE"; "$SLOPBOX" init --changes live --yes >/dev/null;' : ''} ${scenario === 'pi' ? '"$SLOPBOX" --approval-view' : invocation} </dev/tty & pid=$!; printf '%s' "$pid" >"$PID_FILE"; wait "$pid"; result=$?; test "$(stty -g)" = "$before" || exit 90; printf '\\nLIFECYCLE_RESTORED:%s\\n' "$result"; exit "$result"`;
  const child = spawn('script', ['-q', '-e', '-c', command, '/dev/null'], {
    env: { ...process.env, SLOPBOX: binary, VIEW_WORKSPACE: workspace, PID_FILE: pidFile, MODE_FILE: modeFile },
    stdio: ['pipe', 'pipe', 'pipe'],
  });
  let output = '';
  let pending = '';
  let ended = false;
  let supervisor;
  const closed = new Promise(resolve => child.on('close', code => { ended = true; resolve(code); }));
  for (const stream of [child.stdout, child.stderr]) stream.on('data', bytes => { output += bytes; pending += bytes; });
  const deadline = setTimeout(() => child.kill('SIGKILL'), 30000);
  async function waitFor(pattern) {
    const until = Date.now() + 10000;
    while (Date.now() < until) {
      const match = pending.match(pattern);
      if (match) { pending = pending.slice(match.index + match[0].length); return match; }
      if (ended) break;
      await delay(10);
    }
    throw new Error(`${scenario}: timed out waiting for ${pattern}\n${output}`);
  }
  try {
    if (scenario === 'pi') {
      await waitFor(/Ctrl-\] opens host network approvals/);
      await waitFor(/\x1b\[\?2004h/);
    } else {
      await waitFor(/LIFECYCLE_READY/);
    }
    const pid = Number(readFileSync(pidFile, 'utf8'));
    supervisor = pid;
    if (scenario === 'suspend') {
      child.stdin.write('\x1a');
      let stopped = false;
      for (let attempt = 0; attempt < 100; attempt++) {
        if (/^State:\s+T/m.test(readFileSync(`/proc/${pid}/status`, 'utf8'))) { stopped = true; break; }
        await delay(20);
      }
      assert(stopped, 'Ctrl-Z did not suspend the supervisor');
      assert.equal(execFileSync('stty', ['-g', '-F', readFileSync(`${modeFile}.tty`, 'utf8').trim()], { encoding: 'utf8' }).trim(), readFileSync(modeFile, 'utf8'));
      process.kill(pid, 'SIGCONT');
      await delay(100);
    }
    child.stdin.write('\x1d');
    await waitFor(/SLOPBOX HOST APPROVALS/);
    await waitFor(/\r\n> /);
    if (scenario === 'signal') {
      process.kill(pid, 'SIGTERM');
      await waitFor(/LIFECYCLE_RESTORED:143/);
      assert.equal(await closed, 143);
    } else {
      child.stdin.write('q\n');
      await delay(350);
      if (scenario === 'pi') {
        // Returning triggers a resize redraw. No model request is needed.
        await waitFor(/\x1b\[\?2026h/);
        process.kill(pid, 'SIGTERM');
        await waitFor(/LIFECYCLE_RESTORED:143/);
        assert.equal(await closed, 143);
      } else {
        child.stdin.write('finish\n');
        await waitFor(/LIFECYCLE_DONE/);
        await waitFor(/LIFECYCLE_RESTORED:0/);
        assert.equal(await closed, 0);
      }
    }
    console.log(`e2e: terminal ${scenario} lifecycle passed`);
  } finally {
    clearTimeout(deadline);
    if (!ended) {
      if (supervisor) {
        try { process.kill(supervisor, 'SIGCONT'); process.kill(supervisor, 'SIGTERM'); } catch {}
      }
      child.kill('SIGTERM');
      await Promise.race([closed, delay(1000)]);
      if (!ended) child.kill('SIGKILL');
    }
  }
}
