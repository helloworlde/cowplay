// Exercise the real Chrome startup path while the DevTools file is empty or incomplete.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import childProcess from 'node:child_process';
import {syncBuiltinESMExports} from 'node:module';

const originalSpawn = childProcess.spawn, originalRead = fs.readFileSync;
const children = [], profiles = [], incomplete = ['', '123', '12345\n'];
let reads = 0, failRead = false, browser;
childProcess.spawn = function(command, args, options) {
  const child = originalSpawn(command, args, options);
  const profile = args?.find(arg => arg.startsWith('--user-data-dir='));
  if (profile) { children.push(child); profiles.push(profile.slice('--user-data-dir='.length)); }
  return child;
};
syncBuiltinESMExports();
fs.readFileSync = function(file, ...args) {
  if (path.basename(String(file)) === 'DevToolsActivePort') {
    if (failRead) throw Object.assign(new Error('synthetic DevTools file read failure'), {code: 'EACCES'});
    if (reads < incomplete.length) return incomplete[reads++];
  }
  return originalRead(file, ...args);
};
try {
  const {start} = await import('./lib/ui-browser.mjs');
  browser = await start();
  assert.equal(reads, incomplete.length, 'wait for the complete port and browser endpoint');
  assert.equal(await browser.evaluate('6 * 7'), 42, 'the resulting Chrome connection must work');
  await browser.close(); browser = null;
  failRead = true;
  await assert.rejects(start, {code: 'EACCES'}, 'a real startup failure remains visible');
  assert.ok(children.at(-1).exitCode !== null || children.at(-1).signalCode !== null, 'failed startup stops its Chrome process');
  assert.equal(fs.existsSync(profiles.at(-1)), false, 'failed startup removes only its own temporary profile');
  console.log(JSON.stringify({passed: true, checks: ['empty DevTools file', 'partially written port', 'missing browser endpoint', 'real Chrome evaluation', 'startup failure propagates and cleans up']}));
} finally {
  fs.readFileSync = originalRead;
  childProcess.spawn = originalSpawn;
  syncBuiltinESMExports();
  if (browser) await browser.close();
  for (const child of children) if (child.exitCode === null && child.signalCode === null) {
    child.kill('SIGTERM');
    await new Promise(resolve => child.once('exit', resolve));
  }
  for (const profile of profiles) fs.rmSync(profile, {recursive: true, force: true, maxRetries: 3, retryDelay: 100});
}
