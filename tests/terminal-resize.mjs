import assert from 'node:assert/strict';
import { spawn, execFileSync } from 'node:child_process';
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { setTimeout as delay } from 'node:timers/promises';

const [binary, root] = process.argv.slice(2);
const workspace = `${root}/resize-workspace`;
mkdirSync(workspace);
writeFileSync(`${workspace}/probe.cjs`, `
process.stdin.setRawMode(true);
const report = () => process.stdout.write('SIZE:' + process.stdout.columns + ',' + process.stdout.rows + '\\n');
report();
process.stdout.on('resize', report);
process.stdin.on('data', bytes => {
  if (bytes.includes(113)) process.exit(0);
  process.stdout.write('KEY:' + bytes.toString('hex') + '\\n');
});
`);
for (const mode of ['plain', 'approval']) {
  const ttyFile = `${root}/resize-${mode}.tty`;
  const command = `stty cols 100 rows 30; tty > "$TTY_FILE"; exec "$SLOPBOX" run ${mode === 'approval' ? '--approval-view' : ''} --workspace "$VIEW_WORKSPACE" --dev-env none -- node "$VIEW_WORKSPACE/probe.cjs"`;
  const child = spawn('script', ['-q', '-e', '-c', command, '/dev/null'], {
    env: { ...process.env, SLOPBOX: binary, VIEW_WORKSPACE: workspace, TTY_FILE: ttyFile },
    stdio: ['pipe', 'pipe', 'pipe'],
  });
  let output = '';
  let pending = '';
  let ended = false;
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
    throw new Error(`${mode}: timed out waiting for ${pattern}\n${output}`);
  }
  try {
    await waitFor(/SIZE:100,30/);
    const tty = readFileSync(ttyFile, 'utf8').trim();
    execFileSync('stty', ['-F', tty, 'cols', '70', 'rows', '20']);
    await waitFor(/SIZE:70,20/);
    execFileSync('stty', ['-F', tty, 'rows', '40']);
    await waitFor(/SIZE:70,40/);
    for (const columns of ['80', '90', '120']) execFileSync('stty', ['-F', tty, 'cols', columns, 'rows', '28']);
    await waitFor(/SIZE:120,28/);
    if (mode === 'plain') {
      child.stdin.write('\x1d');
      await waitFor(/KEY:1d/);
      assert(!output.includes('SLOPBOX HOST APPROVALS'));
      assert(!output.includes('Ctrl-] opens host network approvals'));
    } else {
      child.stdin.write('\x1d');
      await waitFor(/SLOPBOX HOST APPROVALS/);
      await waitFor(/\r\n> /);
      execFileSync('stty', ['-F', tty, 'cols', '100', 'rows', '26']);
      await waitFor(/SLOPBOX HOST APPROVALS/);
      await waitFor(/\r\n> /);
      child.stdin.write('q\n');
      await waitFor(/SIZE:100,26/);
      await delay(300);
    }
    child.stdin.write('q');
    assert.equal(await closed, 0);
    console.log(`e2e: ${mode} terminal forwards width, height, and rapid resizes`);
  } finally {
    clearTimeout(deadline);
    if (!ended) child.kill('SIGTERM');
  }
}
