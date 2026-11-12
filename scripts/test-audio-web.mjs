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
const wireTrace = process.env.COWPLAY_AUDIO_WIRE_TRACE ? JSON.parse(fs.readFileSync(process.env.COWPLAY_AUDIO_WIRE_TRACE, 'utf8')) : null;
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
    const wireTrace = ${JSON.stringify(wireTrace)};
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
      cases.push({name: '64-source limit drops only the late burst and keeps queued audio playing', pass: result.active === 6144 && result.runs.length === 1 && result.runs[0].start === 5760 && e.audio.counters.drops === 6 && e.errors.length === 0, result, errors: e.errors});
    }
    {
      const e = engine(); await e.audio.unlock(); e.audio.handleConfig(config());
      for (let i = 0; i < 13; i++) e.audio.handlePacket(packet(960, i * 20000));
      const result = await render();
      cases.push({name: '350 ms playback limit drops only the late burst and keeps queued audio playing', pass: result.active === 11520 && result.runs.length === 1 && result.runs[0].start === 5760 && e.audio.counters.drops === 1 && e.errors.length === 0, result, errors: e.errors});
    }
    {
      // Music (type 102) gets a deeper jitter buffer: a 200 ms arrival gap stays continuous.
      const e = engine(); await e.audio.unlock();
      const bytes = new Uint8Array([3, 102, 3, 0x80, 0xbb, 0, 0, 2, 5, ...new TextEncoder().encode('media')]);
      e.audio.handleConfig(parseWireMessage(bytes.buffer));
      const music = (timestampUs) => { const m = packet(960, timestampUs); m.streamType = 102; return m; };
      e.audio.handlePacket(music(0)); arrivalTime = 0.2; e.audio.handlePacket(music(20000));
      const result = await render();
      cases.push({name: 'music stream rides out a 200 ms arrival gap without an underrun', pass: result.active === 1920 && result.runs.length === 1 && result.runs[0].start === 12000 && e.audio.counters.underruns === 0 && e.errors.length === 0, result, errors: e.errors});
    }
    {
      const encoded=[], failures=[]; let description;
      const encoder=new AudioEncoder({output:(chunk,metadata)=>{const data=new Uint8Array(chunk.byteLength);chunk.copyTo(data);encoded.push({data,timestamp:chunk.timestamp});if(metadata.decoderConfig?.description)description=new Uint8Array(metadata.decoderConfig.description);},error:error=>failures.push(error.message)});
      encoder.configure({codec:'mp4a.40.2',sampleRate:48000,numberOfChannels:2,bitrate:128000});
      for(let i=0;i<10;i++){
        const samples=new Float32Array(2048);
        for(let n=0;n<1024;n++)samples[n]=samples[n+1024]=0.25*Math.sin(2*Math.PI*440*(i*1024+n)/48000);
        const data=new AudioData({format:'f32-planar',sampleRate:48000,numberOfChannels:2,numberOfFrames:1024,timestamp:Math.round(i*1024e6/48000),data:samples});
        encoder.encode(data);data.close();
      }
      await encoder.flush();encoder.close();
      const NativeDecoder=AudioDecoder;let decoded;
      window.AudioDecoder=class extends NativeDecoder {constructor(init){super({...init,output:data=>{init.output(data);decoded?.();}});}};
      try {
        const e=engine();await e.audio.unlock();
        e.audio.handleConfig({streamType:102,codec:1,sampleRate:48000,channels:2,name:'media',description});
        for(const packet of encoded){
          arrivalTime=packet.timestamp/1e6;
          const ready=new Promise((resolve,reject)=>{const timeout=setTimeout(()=>reject(Error('native AAC output timed out')),2000);decoded=()=>{clearTimeout(timeout);resolve();};});
          e.audio.handlePacket({streamType:102,timestampUs:packet.timestamp,data:packet.data});await ready;
        }
        const buffer=await context.startRendering(), samples=buffer.getChannelData(0);
        let minWindowEnergy=Infinity;
        // Ignore encoder priming and tail; the steady signal must contain no missing 128-frame window.
        // Type 102 music starts after its 250 ms jitter buffer (12000 frames at 48 kHz).
        for(let start=12000+2048;start<12000+10*1024;start+=128){let sum=0;for(let i=start;i<start+128;i++)sum+=samples[i]*samples[i];minWindowEnergy=Math.min(minWindowEnergy,sum/128);}
        const result={packets:encoded.length,minWindowEnergy,errors:[...e.errors,...failures]};
        cases.push({name:'native AAC stereo packets retain continuous Web Audio playback',pass:encoded.length>=10&&minWindowEnergy>0.001&&e.errors.length===0&&failures.length===0,result});
        if (wireTrace) {
          const mixed=engine();await mixed.audio.unlock();
          mixed.audio.setVolume(0.5);mixed.audio.setStreamGain('media',0.5);
          mixed.audio.handleConfig({streamType:102,codec:1,sampleRate:48000,channels:2,name:'media',description});
          for(let i=0;i<encoded.length;i++) {
            for(const bytes of wireTrace[i % wireTrace.length].wire_messages) {
              const message=parseWireMessage(Uint8Array.from(bytes).buffer);
              if(bytes[0]===6)mixed.audio.stopStream(message.streamType);
              else if(bytes[0]===3)mixed.audio.handleConfig(message);
            }
            const packet=encoded[i];arrivalTime=packet.timestamp/1e6;
            const ready=new Promise((resolve,reject)=>{const timeout=setTimeout(()=>reject(Error('native AAC output timed out after prompt transition')),2000);decoded=()=>{clearTimeout(timeout);resolve();};});
            mixed.audio.handlePacket({streamType:102,timestampUs:packet.timestamp,data:packet.data});await ready;
          }
          const mixedBuffer=await context.startRendering(),mixedSamples=mixedBuffer.getChannelData(0);
          let maxSampleError=0;
          for(let i=0;i<samples.length;i++)maxSampleError=Math.max(maxSampleError,Math.abs(mixedSamples[i]-samples[i]*0.25));
          const result={transitions:encoded.length,maxSampleError,underruns:mixed.audio.counters.underruns,errors:mixed.errors};
          cases.push({name:'actual server prompt transitions preserve native AAC music samples and gain',pass:maxSampleError<0.000001&&mixed.audio.counters.underruns===0&&mixed.errors.length===0,result});
        }
      } finally {window.AudioDecoder=NativeDecoder;}
    }
    {
      // Exercise the actual decoder limit, then send more audio after the burst.
      const encoded=[], failures=[]; let description;
      const encoder=new AudioEncoder({output:(chunk,metadata)=>{const data=new Uint8Array(chunk.byteLength);chunk.copyTo(data);encoded.push({data,timestamp:chunk.timestamp});if(metadata.decoderConfig?.description)description=new Uint8Array(metadata.decoderConfig.description);},error:error=>failures.push(error.message)});
      encoder.configure({codec:'mp4a.40.2',sampleRate:48000,numberOfChannels:2,bitrate:128000});
      for(let i=0;i<100;i++){
        const samples=new Float32Array(2048);
        for(let n=0;n<1024;n++)samples[n]=samples[n+1024]=0.25*Math.sin(2*Math.PI*440*(i*1024+n)/48000);
        const data=new AudioData({format:'f32-planar',sampleRate:48000,numberOfChannels:2,numberOfFrames:1024,timestamp:Math.round(i*1024e6/48000),data:samples});
        encoder.encode(data);data.close();
      }
      await encoder.flush();encoder.close();
      const NativeDecoder=AudioDecoder,decoders=[];let outputs=0;
      window.AudioDecoder=class extends NativeDecoder {
        constructor(init){
          super({...init,output:data=>{init.output(data);outputs++;}});decoders.push(this);
          this.fail=()=>{if(this.state!=='closed')this.close();init.error(new DOMException('synthetic fatal audio error','EncodingError'));};
        }
      };
      try {
        const e=engine();await e.audio.unlock();arrivalTime=0;
        e.audio.handleConfig({streamType:102,codec:1,sampleRate:48000,channels:2,name:'media',description});
        for(const packet of encoded)e.audio.handlePacket({streamType:102,timestampUs:packet.timestamp,data:packet.data});
        await decoders.at(-1).flush();
        const burstOutputs=outputs,burstDrops=e.audio.counters.drops;
        arrivalTime=0.6;
        e.audio.handlePacket({streamType:102,timestampUs:encoded[90].timestamp,data:encoded[90].data});
        await decoders.at(-1).flush();
        const buffer=await context.startRendering(), samples=buffer.getChannelData(0);
        let minWindowEnergy=Infinity;
        for(let start=12000+2048;start<12000+20*1024;start+=128){let sum=0;for(let i=start;i<start+128;i++)sum+=samples[i]*samples[i];minWindowEnergy=Math.min(minWindowEnergy,sum/128);}
        let continuationEnergy=0;for(let i=37000;i<37500;i++)continuationEnergy+=samples[i]*samples[i];
        const result={packets:encoded.length,burstOutputs,outputs,minWindowEnergy,continuationEnergy,burstDrops,errors:[...e.errors,...failures]};
        cases.push({name:'AAC decoder overflow drops excess packets and continues playing later audio',pass:encoded.length>=100&&burstOutputs===64&&burstDrops>=encoded.length-64&&outputs===65&&minWindowEnergy>0.001&&continuationEnergy>0.1&&e.errors.length===0&&failures.length===0,result});

        const recovering=engine();await recovering.audio.unlock();
        const config={streamType:102,codec:1,sampleRate:48000,channels:2,name:'media',description};
        const before=decoders.length;
        recovering.audio.handleConfig(config);decoders.at(-1).fail();
        const beforeOutput=outputs;
        for(const packet of encoded.slice(0,10))recovering.audio.handlePacket({streamType:102,timestampUs:packet.timestamp,data:packet.data});
        if(decoders.at(-1).state!=='closed')await decoders.at(-1).flush();
        const recoveredOutputs=outputs-beforeOutput;
        const recoveredBuffer=await context.startRendering();
        let recoveredEnergy=0;for(const value of recoveredBuffer.getChannelData(0))recoveredEnergy+=value*value;
        const recoveredInstances=decoders.length-before;
        decoders.at(-1).fail();
        recovering.audio.handlePacket({streamType:102,timestampUs:encoded[10].timestamp,data:encoded[10].data});
        const bounded=decoders.length-before===2&&recovering.errors.some(s=>s.includes('重新连接'));
        recovering.audio.handleConfig(config);const stale=decoders.at(-1);
        recovering.audio.stopAll();const beforeStale=decoders.length,errorCount=recovering.errors.length;stale.fail();
        const stopped=decoders.length===beforeStale&&recovering.errors.length===errorCount;
        const recoveryResult={recoveredOutputs,recoveredEnergy,recoveredInstances,bounded,stopped,errors:recovering.errors};
        cases.push({name:'fatal AAC decoder error recovers once, stays bounded and ignores errors after stop',pass:recoveredOutputs===10&&recoveredEnergy>1&&recoveredInstances===2&&bounded&&stopped,result:recoveryResult});
      } finally {window.AudioDecoder=NativeDecoder;}
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
