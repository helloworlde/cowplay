// Native Web Audio regression with synthetic PCM. No phone or physical speaker is used.
import assert from 'node:assert/strict';
import {execFileSync} from 'node:child_process';
import fs from 'node:fs';
import http from 'node:http';
import os from 'node:os';
import path from 'node:path';
import {start} from './lib/ui-browser.mjs';

const root = path.resolve(import.meta.dirname, '..');
const temp = fs.mkdtempSync(path.join(os.tmpdir(), 'cowplay-audio-web-'));
const output = process.env.COWPLAY_UI_OUTPUT ?? temp;
const tsc = process.env.TSC_JS ?? path.join(root, 'web/node_modules/typescript/bin/tsc');
let browser;
const server = http.createServer((req, res) => {
  if (req.url === '/favicon.ico') { res.writeHead(204); res.end(); return; }
  if (req.url === '/') {
    res.setHeader('Content-Type', 'text/html');
    res.end('<!doctype html><title>CowPlay synthetic audio regression</title>');
    return;
  }
  const module = {'/audio.js': 'audio.js', '/protocol': 'protocol.js'}[req.url];
  if (!module) { res.writeHead(404); res.end(); return; }
  res.setHeader('Content-Type', 'text/javascript');
  res.end(fs.readFileSync(path.join(temp, 'modules/src', module)));
});

try {
  execFileSync(process.execPath, [tsc, '--project', path.join(root, 'web/tsconfig.json'),
    '--noEmit', 'false', '--outDir', path.join(temp, 'modules')], {stdio: 'pipe'});
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  browser = await start();
  await browser.goto(`http://127.0.0.1:${server.address().port}/`);
  const report = await browser.evaluate(`(async () => {
    const {AudioEngine} = await import('/audio.js');
    const {parseWireMessage} = await import('/protocol');
    let arrivalTime = 0, context;
    // Packet arrival time and the engine's unlock gate are controlled. Native Web Audio
    // still renders every sample offline; this does not test browser autoplay policy.
    window.AudioContext = class {
      constructor() {
        context = new OfflineAudioContext(2, 48000, 48000);
        Object.defineProperty(context, 'currentTime', {get: () => arrivalTime});
        context.resume = async () => {};
        return context;
      }
    };
    function engine() {
      arrivalTime = 0;
      const errors = [];
      return {audio: new AudioEngine(message => errors.push(message)), errors};
    }
    function config() {
      const bytes = new Uint8Array([3, 100, 3, 0x80, 0xbb, 0, 0, 2, 5, ...new TextEncoder().encode('media')]);
      return parseWireMessage(bytes.buffer);
    }
    function packet(frames = 960, timestampUs = 0, left = 8192, right = -8192) {
      const bytes = new Uint8Array(10 + frames * 4), view = new DataView(bytes.buffer);
      bytes.set([4, 100]); view.setBigUint64(2, BigInt(timestampUs), true);
      for (let i = 0; i < frames; i++) {
        view.setInt16(10 + i * 4, left, true);
        view.setInt16(12 + i * 4, right, true);
      }
      return parseWireMessage(bytes.buffer);
    }
    async function render() {
      const buffer = await context.startRendering(), samples = buffer.getChannelData(0);
      const runs = []; let active = 0;
      for (let i = 0; i < samples.length; i++) {
        if (Math.abs(samples[i]) <= 0.000001) continue;
        active++;
        if (runs.at(-1)?.end === i) runs.at(-1).end++;
        else runs.push({start: i, end: i + 1});
      }
      return {active, runs, leftAt125ms: samples[6000], rightAt125ms: buffer.getChannelData(1)[6000]};
    }
    const cases = [];
    {
      const e = engine(); e.audio.handleConfig(config()); e.audio.handlePacket(packet());
      const result = await render();
      cases.push({name: 'locked engine does not render', pass: result.active === 0 && e.errors.length === 0, result, errors: e.errors});
    }
    {
      const e = engine(); await e.audio.unlock(); e.audio.handleConfig(config());
      e.audio.setVolume(0.5); e.audio.handlePacket(packet());
      const result = await render();
      cases.push({name: 'initial buffer, stereo byte order and volume', pass: result.active === 960 && result.runs[0]?.start === 5760 && result.leftAt125ms === 0.125 && result.rightAt125ms === -0.125 && e.errors.length === 0, result, errors: e.errors});
    }
    {
      const e=engine();await e.audio.unlock();e.audio.setStreamGain('media',0.25);e.audio.setVolume(0.5);
      e.audio.handleConfig(config());e.audio.handlePacket(packet());const result=await render();
      cases.push({name:'saved per-stream gain combines with the master after stream setup',pass:result.active===960&&result.leftAt125ms===0.03125&&result.rightAt125ms===-0.03125&&e.errors.length===0,result,errors:e.errors});
    }
    {
      const e = engine(); await e.audio.unlock(); e.audio.handleConfig(config());
      e.audio.handlePacket(packet()); arrivalTime = 0.060; e.audio.handlePacket(packet(960, 20000));
      const result = await render();
      cases.push({name: '60 ms arrival jitter within the initial buffer keeps consecutive samples continuous', pass: result.active === 1920 && result.runs.length === 1 && result.runs[0].start === 5760 && e.errors.length === 0, result, errors: e.errors});
    }
    {
      const e = engine(); await e.audio.unlock(); e.audio.handleConfig(config());
      e.audio.handlePacket(packet()); arrivalTime = 0.2; e.audio.handlePacket(packet(960, 20000));
      const result = await render();
      cases.push({name: 'real underrun restores the initial buffer', pass: result.active === 1920 && result.runs.length === 2 && result.runs[1].start === 15360 && e.errors.length === 0, result, errors: e.errors});
    }
    {
      const e = engine(); await e.audio.unlock(); e.audio.handleConfig(config());
      e.audio.handlePacket(packet()); e.audio.stopStream(100);
      const result = await render();
      cases.push({name: 'stopping cancels queued samples', pass: result.active === 0 && e.errors.length === 0, result, errors: e.errors});
    }
    {
      const e = engine(); await e.audio.unlock(); e.audio.handleConfig(config());
      e.audio.handlePacket(packet()); e.audio.handleConfig(config()); e.audio.handlePacket(packet(960, 0, -16384, 16384));
      const result = await render();
      cases.push({name: 'new configuration cancels the previous stream', pass: result.active === 960 && result.leftAt125ms === -0.5 && result.rightAt125ms === 0.5 && e.errors.length === 0, result, errors: e.errors});
    }
    {
      const e = engine(); await e.audio.unlock(); e.audio.handleConfig(config());
      for (let i = 0; i < 70; i++) e.audio.handlePacket(packet(96, i * 2000));
      const result = await render();
      cases.push({name: '64-source limit still cancels backlog', pass: result.active === 576 && result.runs.length === 1 && e.errors.length === 1 && e.errors[0].includes('backlog reset'), result, errors: e.errors});
    }
    {
      const e = engine(); await e.audio.unlock(); e.audio.handleConfig(config());
      for (let i = 0; i < 13; i++) e.audio.handlePacket(packet(960, i * 20000));
      const result = await render();
      cases.push({name: '350 ms playback limit still cancels backlog', pass: result.active === 960 && result.runs.length === 1 && e.errors.length === 1 && e.errors[0].includes('backlog reset'), result, errors: e.errors});
    }
    return {userAgent: navigator.userAgent, renderer: 'native OfflineAudioContext', physicalSpeakerOutput: false, autoplayPolicyTested: false, cases};
  })()`);
  report.browserErrors = browser.errors;
  fs.mkdirSync(output, {recursive: true});
  fs.writeFileSync(path.join(output, 'audio-results.json'), JSON.stringify(report, null, 2) + '\n');
  for (const c of report.cases) console.log(`${c.pass ? 'PASS' : 'FAIL'} ${c.name}`);
  for (const c of report.cases) assert.equal(c.pass, true, `${c.name}: ${JSON.stringify(c.result)}`);
  assert.deepEqual(browser.errors, []);
  console.log(`${report.cases.length} native offline audio checks passed`);
} finally {
  if (browser) await browser.close();
  await new Promise(resolve => server.close(resolve));
  fs.rmSync(temp, {recursive: true, force: true});
}
