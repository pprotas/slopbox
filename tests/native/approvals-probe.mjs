import assert from 'node:assert/strict';
import fs from 'node:fs';
import net from 'node:net';

process.stdin.setRawMode(true);
const settings = JSON.parse(fs.readFileSync('probe.json', 'utf8'));
let attempts = 0;
function request(text) {
  const proxy = new URL(process.env.HTTP_PROXY);
  return new Promise((resolve, reject) => {
    const socket = net.connect(Number(proxy.port), proxy.hostname, () => socket.end(text));
    let output = '';
    socket.setTimeout(10000, () => socket.destroy(new Error('proxy timeout')));
    socket.on('data', bytes => {
      output += bytes;
      if (output.length > 65536) socket.destroy(new Error('oversized response'));
    });
    socket.on('end', () => resolve(output));
    socket.on('error', reject);
  });
}
async function retry() {
  attempts++;
  const response = await request('GET http://view.slopbox-native.invalid/ HTTP/1.1\r\nHost: view.slopbox-native.invalid\r\nConnection: close\r\n\r\n');
  fs.writeFileSync(`response-${attempts}`, response);
  fs.writeFileSync('attempts', String(attempts));
}
function guards() {
  for (const [path, flags] of [
    [settings.tty, fs.constants.O_RDONLY | fs.constants.O_NONBLOCK],
    [settings.rules, fs.constants.O_RDONLY],
    [settings.rules, fs.constants.O_WRONLY],
  ]) {
    assert.throws(() => fs.closeSync(fs.openSync(path, flags)), {code: 'EPERM'});
  }
  process.stdout.write('GUARDS_DENIED\n');
}
if (process.env.HTTP_PROXY) await retry();
else assert.equal(settings.network, 'none');
process.stdout.write('\x1ds 1\np 1\nyes\nGUEST_FORGED_OUTPUT\n');
process.stdout.write('PROBE_READY\n');
process.stdout.on('resize', () => {
  process.stdout.write(`SIZE:${process.stdout.columns},${process.stdout.rows}\n`);
});
let input = '';
let work = Promise.resolve();
process.stdin.on('data', bytes => {
  fs.appendFileSync('guest-input', bytes);
  if (bytes.includes(0x1d)) process.stdout.write('LITERAL_CTRL_BRACKET\n');
  input += bytes.toString().replaceAll('\x1d', '');
  while (input.includes('\n')) {
    const end = input.indexOf('\n');
    const command = input.slice(0, end);
    input = input.slice(end + 1);
    work = work.then(async () => {
      if (command === 'retry') {
        await retry();
        process.stdout.write('RETRIED\n');
      } else if (command === 'guards') guards();
      else if (command === 'exit') process.exit(0);
      else throw new Error(`unexpected guest input: ${JSON.stringify(command)}`);
    }).catch(error => {console.error(error); process.exit(1);});
  }
});
let busy = false;
setInterval(async () => {
  if (busy) return;
  const action = ['diagnostic', 'fail'].find(value => fs.existsSync(`host-${value}`));
  if (!action) return;
  busy = true;
  try {
    fs.unlinkSync(`host-${action}`);
    if (action === 'diagnostic') {
      await request('BROKEN\r\n\r\n');
      process.stdout.write('GUEST_WHILE_HOST_VIEW\x1dp 1\nyes\n');
      fs.writeFileSync('diagnostic-done', '');
    } else if (action === 'fail') process.exit(7);
    else throw new Error(`unknown host action: ${action}`);
  } catch (error) {
    console.error(error);
    process.exit(1);
  } finally {
    busy = false;
  }
}, 20);
