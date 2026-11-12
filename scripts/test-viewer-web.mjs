// Synthetic viewer input and decoder tests in Chrome. No phone media or identity is used.
import fs from 'node:fs';
import http from 'node:http';
import os from 'node:os';
import path from 'node:path';
import assert from 'node:assert/strict';
import {spawn, execFileSync} from 'node:child_process';
import {chromeExecutable, devToolsPort} from './lib/ui-browser.mjs';
const root=path.resolve(import.meta.dirname,'..');
const temp=fs.mkdtempSync(path.join(os.tmpdir(),'carplay-video-web-'));
const tsc=process.env.TSC_JS ?? path.join(root,'web/node_modules/typescript/bin/tsc');
execFileSync(process.execPath,[tsc,'--project',path.join(root,'web/tsconfig.json'),'--noEmit','false','--outDir',path.join(temp,'modules')],{stdio:'pipe'});
const server=http.createServer((req,res)=>{
 if(req.url==='/favicon.ico'){res.writeHead(204);res.end();return;}
 if(req.url==='/'){res.setHeader('Content-Type','text/html');res.end('<!doctype html><title>CarPlay decoder regression</title><canvas id="screen" width="2" height="2"></canvas>');return;}
 const name=req.url==='/video.js'?'video.js':req.url==='/protocol'?'protocol.js':req.url==='/input.js'?'input.js':null;
 if(!name){res.writeHead(404);res.end();return;}
 res.setHeader('Content-Type','text/javascript');res.end(fs.readFileSync(path.join(temp,'modules/src',name)));
});
await new Promise(r=>server.listen(0,'127.0.0.1',r));
const chrome=spawn(chromeExecutable(),['--headless=new','--remote-debugging-address=127.0.0.1','--remote-debugging-port=0','--user-data-dir='+temp,'--no-first-run','--no-default-browser-check','--disable-background-networking','about:blank'],{stdio:['ignore','ignore',fs.openSync(path.join(temp,'chrome.log'),'a')]});
let ws;const browserErrors=[],browserWarnings=[];
try{
 async function wait(fn,label){const end=Date.now()+8000;while(Date.now()<end){if(await fn())return;await new Promise(r=>setTimeout(r,50));}throw Error('timeout: '+label);}
 let port;await wait(()=>{port=devToolsPort(temp);return port!==null;},'CDP');
 const targets=await(await fetch(`http://127.0.0.1:${port}/json/list`)).json();
 ws=new WebSocket(targets.find(t=>t.type==='page').webSocketDebuggerUrl);
 await new Promise((r,j)=>{ws.onopen=r;ws.onerror=j});
 let id=0;const pending=new Map();
 ws.onmessage=async e=>{const m=JSON.parse(e.data);if(m.method==='Runtime.exceptionThrown')browserErrors.push(m.params.exceptionDetails.text);if(m.method==='Runtime.consoleAPICalled'&&['error','warning'].includes(m.params.type))(m.params.type==='error'?browserErrors:browserWarnings).push(m.params.args.map(a=>a.value??a.description??'').join(' '));if(m.method==='Fetch.requestPaused'){const pathname=new URL(m.params.request.url).pathname;if(pathname==='/favicon.ico'){await call('Fetch.fulfillRequest',{requestId:m.params.requestId,responseCode:204});return;}const name=pathname==='/'?'index.html':pathname.slice(1);const file=path.join(root,'web/dist',name);const found=!name.includes('..')&&fs.existsSync(file)&&fs.statSync(file).isFile();await call('Fetch.fulfillRequest',{requestId:m.params.requestId,responseCode:found?200:404,responseHeaders:[{name:'Content-Type',value:name.endsWith('.js')?'text/javascript':name.endsWith('.css')?'text/css':'text/html'}],body:found?fs.readFileSync(file).toString('base64'):''});return;}if(m.id){pending.get(m.id)?.(m);pending.delete(m.id)}};
 const call=(method,params={})=>new Promise((r,j)=>{pending.set(++id,m=>m.error?j(Error(JSON.stringify(m.error))):r(m.result));ws.send(JSON.stringify({id,method,params}));});
 await call('Page.enable');await call('Runtime.enable');
 await call('Page.navigate',{url:`http://127.0.0.1:${server.address().port}/`});
 const evaluate=async (expression,gesture=false)=>{const r=await call('Runtime.evaluate',{expression,returnByValue:true,awaitPromise:true,userGesture:gesture});if(r.exceptionDetails)throw Error(r.exceptionDetails.text);return r.result.value;};
 await wait(()=>evaluate("!!document.querySelector('#screen')"),'test canvas');
 const result=await evaluate(`(async()=>{
   const {VideoPlayer}=await import('/video.js');
   const instances=[];let delay=2800;
   const pixels=new OffscreenCanvas(2,2);pixels.getContext('2d').fillRect(0,0,2,2);
   class Decoder {
     state='unconfigured';decodeQueueSize=0;timers=[];
     constructor(init){this.init=init;instances.push(this);}
     configure(){this.state='configured';}
     static async isConfigSupported(config){return {supported:true,config};}
     decode(){const timer=setTimeout(()=>{if(this.state==='configured')this.init.output(new VideoFrame(pixels,{timestamp:0}));},delay);this.timers.push(timer);}
     close(){this.state='closed';for(const timer of this.timers)clearTimeout(timer);}
     reject(){this.close();this.init.error(new DOMException('synthetic decoder failure','EncodingError'));}
   }
   window.VideoDecoder=Decoder;
   const config={kind:'video-config',codec:1,streamType:110,width:2,height:2,codecData:new Uint8Array([1,66,0,30,255,225,0])};
   const copy=()=>({...config,codecData:config.codecData.slice()});
   const frame={kind:'video-frame',streamType:110,keyframe:true,timestampUs:0,data:new Uint8Array([0,0,0,1,101,1])};
   let frames=0,requests=0;const errors=[];
   const player=new VideoPlayer(document.querySelector('#screen'),message=>{if(message)errors.push(message)},()=>frames++,()=>{requests++;player.configure(copy());player.pushFrame(frame)});
   player.configure(config);player.pushFrame(frame);
   await new Promise(r=>setTimeout(r,3400));
   const slow={frames,requests,decoders:instances.length};
   if(frames===0){player.reset();return {slow};}
   player.reset();delay=0;
   player.configure(copy());player.pushFrame(frame);
   await new Promise(r=>setTimeout(r,20));
   const before=frames;
   instances.at(-1).reject(); // Hardware failure selects the existing fallback.
   instances.at(-1).reject(); // Fallback closes and asks the server for config + IDR.
   await new Promise(r=>setTimeout(r,20));
   const recovered=frames>before&&player.isConfigured;
   player.reset();
   player.configure({...copy(),codec:2,codecData:new Uint8Array([1,1,96,0,0,0,176,0,0,0,0,0,120,0,0,0,0,0,0,0,0,255,0])});
   await new Promise(r=>setTimeout(r,20));
   const hevc={configured:player.isConfigured,codec:player.codecName};
   player.reset();return {slow,recovered,hevc,errors};
 })()`);
 assert.ok(result.slow.frames>0,'a healthy slow decoder must survive unchanged config replay');
 assert.equal(result.slow.decoders,1,'unchanged configuration must retain the healthy decoder');
 assert.equal(result.recovered,true,'closed fallback decoder must recover with the same configuration');
 assert.deepEqual(result.hevc,{configured:true,codec:'hvc1.1.6.L120.B0'},'HEVC diagnostics must report the adopted decoder');
 assert.ok(result.errors.some(message=>message.includes('EncodingError')));
 await evaluate(`(async()=>{
   const {InputController}=await import('/input.js');
   const canvas=document.querySelector('#screen');canvas.style.width='400px';canvas.style.height='240px';
   window.inputMessages=[];
   const input=new InputController(canvas,message=>window.inputMessages.push(message));input.attach();input.setEnabled(true);
 })()`);
 await call('Input.dispatchMouseEvent',{type:'mousePressed',x:208,y:128,button:'left',clickCount:1});
 await new Promise(r=>setTimeout(r,50));
 await call('Input.dispatchMouseEvent',{type:'mouseReleased',x:208,y:128,button:'left',clickCount:1});
 await new Promise(r=>setTimeout(r,50));
 const touch=await evaluate('window.inputMessages');
 const down=touch.find(message=>message.contacts.some(contact=>contact.down));
 const up=touch.find(message=>message.contacts.some(contact=>!contact.down));
 assert.ok(down,'browser press must reach touch input');
 assert.ok(up,'browser release must preserve the contact coordinates');
 assert.equal(up.contacts[0].x,down.contacts[0].x);
 assert.equal(up.contacts[0].y,down.contacts[0].y);
 await evaluate("window.inputMessages=[]");
 await call('Input.dispatchMouseEvent',{type:'mousePressed',x:208,y:128,button:'left',clickCount:1});
 await new Promise(r=>setTimeout(r,50));
 // Drag outside the letterboxed image must cancel rather than commit a tap.
 await call('Input.dispatchMouseEvent',{type:'mouseMoved',x:20,y:128,button:'left',buttons:1});
 await call('Input.dispatchMouseEvent',{type:'mouseReleased',x:20,y:128,button:'left',clickCount:1});
 await new Promise(r=>setTimeout(r,50));
 const cancelled=await evaluate('window.inputMessages');
 assert.ok(cancelled.some(message=>message.contacts.length===0));
 assert.equal(cancelled.some(message=>message.contacts.some(contact=>!contact.down)),false);
 // Hold only application RAF callbacks, keeping browser-native input events real.
 // This deterministically puts release/cancel and the next press in one app frame.
 await evaluate(`(()=>{
   const queued=new Map();let id=100000;
   window.requestAnimationFrame=callback=>{queued.set(++id,callback);return id};
   window.cancelAnimationFrame=id=>queued.delete(id);
   window.drainInputFrame=()=>{for(const [id,callback] of [...queued]){queued.delete(id);callback(performance.now())}};
   window.inputMessages=[];
 })()`);
 await call('Input.dispatchMouseEvent',{type:'mousePressed',x:208,y:128,button:'left',clickCount:1});
 await call('Input.dispatchMouseEvent',{type:'mouseReleased',x:208,y:128,button:'left',clickCount:1});
 const quick=await evaluate('window.inputMessages');
 assert.ok(quick.some(message=>message.contacts.some(contact=>contact.down)));
 assert.ok(quick.some(message=>message.contacts.some(contact=>!contact.down)));
 await evaluate('window.inputMessages=[]');
 await call('Input.dispatchMouseEvent',{type:'mousePressed',x:208,y:128,button:'left',clickCount:1});
 await evaluate('window.drainInputFrame()');
 await call('Input.dispatchMouseEvent',{type:'mouseMoved',x:20,y:128,button:'left',buttons:1});
 await call('Input.dispatchMouseEvent',{type:'mouseReleased',x:20,y:128,button:'left',clickCount:1});
 await call('Input.dispatchMouseEvent',{type:'mousePressed',x:208,y:128,button:'left',clickCount:1});
 const boundary=await evaluate('window.inputMessages');
 assert.ok(boundary.some(message=>message.contacts.length===0),'cancel must reach the phone before a new contact reuses its slot');
 await call('Input.dispatchMouseEvent',{type:'mouseReleased',x:208,y:128,button:'left',clickCount:1});
 await evaluate('window.inputMessages=[]');
 await call('Emulation.setTouchEmulationEnabled',{enabled:true,maxTouchPoints:2});
 await call('Input.dispatchTouchEvent',{type:'touchStart',touchPoints:[{id:1,x:208,y:128},{id:2,x:248,y:128}]});
 await call('Input.dispatchTouchEvent',{type:'touchEnd',touchPoints:[]});
 const multi=await evaluate('window.inputMessages');
 assert.ok(multi.some(message=>message.contacts.length===2&&message.contacts.every(contact=>contact.down)));
 const released=new Set(multi.flatMap(message=>message.contacts.filter(contact=>!contact.down).map(contact=>contact.id)));
 assert.deepEqual([...released].sort(),[0,1]);
 assert.deepEqual(browserErrors,[]);
 console.log(JSON.stringify({passed:true,synthetic:true,checks:['slow-first-frame-keeps-decoder','closed-fallback-recovers-same-config','tap-release-keeps-coordinates','letterbox-drag-cancels','quick-tap-before-frame','cancel-before-slot-reuse','two-touch-release'],browserWarnings,...result}));
}finally{ws?.close();chrome.kill('SIGTERM');await new Promise(r=>chrome.exitCode===null?chrome.once('exit',r):r());server.close();}

await import('./test-session-web.mjs');
await import('./test-connect-web.mjs');
await import('./test-display-web.mjs');
await import('./test-control-flow-web.mjs');
