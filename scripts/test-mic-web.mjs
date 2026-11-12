// Synthetic capture with native WebCodecs. Does not open a microphone or physical speaker.
import assert from 'node:assert/strict';
import {execFileSync} from 'node:child_process';
import fs from 'node:fs';
import http from 'node:http';
import os from 'node:os';
import path from 'node:path';
import {start} from './lib/ui-browser.mjs';
const root=path.resolve(import.meta.dirname,'..');
const temp=fs.mkdtempSync(path.join(os.tmpdir(),'cowplay-mic-web-'));
const output=process.env.COWPLAY_UI_OUTPUT??temp;
let browser;
const server=http.createServer((req,res)=>{
  if(req.url==='/favicon.ico'){res.writeHead(204);res.end();return;}
  if(req.url==='/'){res.end('<!doctype html><title>CowPlay synthetic microphone regression</title>');return;}
  if(req.url!='/mic.js'){res.writeHead(404);res.end();return;}
  res.setHeader('Content-Type','text/javascript');res.end(fs.readFileSync(path.join(temp,'modules/src/car/mic.js')));
});
try {
  execFileSync(process.execPath,[path.join(root,'web/node_modules/typescript/bin/tsc'),'--project',path.join(root,'web/tsconfig.json'),'--noEmit','false','--outDir',path.join(temp,'modules')],{stdio:'pipe'});
  await new Promise(r=>server.listen(0,'127.0.0.1',r));
  browser=await start();await browser.goto(`http://127.0.0.1:${server.address().port}/`);
  const report=await browser.evaluate(`(async()=>{
    const {MicUplink}=await import('/mic.js');
    const NativeEncoder=AudioEncoder, cases=[], encoders=[];
    let node,stopped=0,closed=0,captures=0;
    const stream=()=>({getTracks:()=>[{stop:()=>stopped++}]});
    const capture=async()=>{captures++;return stream();};
    Object.defineProperty(navigator,'mediaDevices',{value:{getUserMedia:capture},configurable:true});
    window.AudioContext=class {
      destination={};resume=async()=>{};close=async()=>{closed++;};
      createMediaStreamSource(){return {connect:()=>{}};}
      createScriptProcessor(){return node={onaudioprocess:null,connect:()=>{},disconnect:()=>{}};}
    };
    window.AudioEncoder=class extends NativeEncoder {constructor(init){super(init);encoders.push(this);}};
    const request=(codec,epoch=41)=>({codec,epoch,rate:codec==='opus'?48000:16000,channels:1,samplesPerPacket:codec==='opus'?960:320,bitrate:64000});
    const feed=(n,value=0.25)=>node.onaudioprocess({inputBuffer:{getChannelData:()=>new Float32Array(n).fill(value)}});
    {
      const sent=[],status=[],mic=new MicUplink(b=>sent.push(new Uint8Array(b)),s=>status.push(s));
      await mic.start(request('lpcm'));feed(1024);feed(256,-0.25);
      const beforeStop=sent.length;mic.stop();
      cases.push({name:'PCM accumulates exact 20 ms frames with little-endian epoch and samples',pass:beforeStop===4&&sent.every(b=>b[0]===8&&new DataView(b.buffer).getBigUint64(1,true)===41n&&b.length===649)&&new DataView(sent[0].buffer).getInt16(9,true)===8192&&!mic.active&&stopped===1&&closed===1,result:{packets:beforeStop,status}});
    }
    {
      const sent=[],status=[],errors=[],mic=new MicUplink(b=>sent.push(new Uint8Array(b)),s=>status.push(s));
      await mic.start(request('opus',42));
      for(let i=0;i<10;i++)feed(960,Math.sin(i)*0.25);
      await encoders.at(-1).flush();
      let samples=0,energy=0;
      const decoder=new AudioDecoder({output:data=>{const values=new Float32Array(data.numberOfFrames);data.copyTo(values,{planeIndex:0,format:'f32-planar'});samples+=values.length;for(const v of values)energy+=v*v;data.close();},error:e=>errors.push(e.message)});
      decoder.configure({codec:'opus',sampleRate:48000,numberOfChannels:1});
      sent.forEach((b,i)=>decoder.decode(new EncodedAudioChunk({type:'key',timestamp:i*20000,data:b.subarray(9)})));
      await decoder.flush();decoder.close();
      const beforeStop=sent.length;mic.stop();
      cases.push({name:'native Opus packets decode, carry their epoch and retain the 48 kHz 20 ms clock',pass:sent.length>=10&&sent.every(b=>b[0]===9&&new DataView(b.buffer).getBigUint64(1,true)===42n)&&samples===sent.length*960&&energy>1&&errors.length===0&&!mic.active&&stopped===2&&closed===2,result:{packets:beforeStop,samples,rms:Math.sqrt(energy/samples),errors,status}});
    }
    {
      const sent=[],status=[],mic=new MicUplink(b=>sent.push(b),s=>status.push(s));
      await mic.start(request('opus',43));feed(960);mic.stop();await new Promise(r=>setTimeout(r,30));
      cases.push({name:'stop drops queued native encoder output',pass:sent.length===0&&!mic.active&&stopped===3&&closed===3,result:{packets:sent.length,status}});
    }
    {
      let resolveCapture;const status=[],mic=new MicUplink(()=>{},s=>status.push(s));
      navigator.mediaDevices.getUserMedia=()=>new Promise(r=>resolveCapture=r);
      const starting=mic.start(request('lpcm',44));mic.stop();resolveCapture(stream());await starting;
      cases.push({name:'late permission result releases its stream after stop',pass:!mic.active&&stopped===4&&closed===3,result:{stopped,closed,status}});
    }
    {
      navigator.mediaDevices.getUserMedia=capture;window.AudioEncoder=undefined;
      const before=captures,status=[],mic=new MicUplink(()=>{},s=>status.push(s));await mic.start(request('opus',45));
      cases.push({name:'unsupported Opus fails before capture and never sends PCM under an Opus negotiation',pass:captures===before&&!mic.active&&status.some(s=>s.includes('不支持 Opus')),result:{captures,status}});
    }
    return {userAgent:navigator.userAgent,physicalMicrophone:false,physicalSpeaker:false,cases};
  })()`);
  report.browserErrors=browser.errors;fs.mkdirSync(output,{recursive:true});fs.writeFileSync(path.join(output,'mic-results.json'),JSON.stringify(report,null,2)+'\n');
  for(const c of report.cases)console.log(`${c.pass?'PASS':'FAIL'} ${c.name}`);
  for(const c of report.cases)assert.equal(c.pass,true,JSON.stringify(c));
  assert.deepEqual(browser.errors,[]);console.log(`${report.cases.length} microphone checks passed`);
} finally {
  if(browser)await browser.close();await new Promise(r=>server.close(r));fs.rmSync(temp,{recursive:true,force:true});
}
