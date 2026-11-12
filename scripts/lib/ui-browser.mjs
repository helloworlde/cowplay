// Reuse installed Chrome/CDP with an isolated temporary profile; no browser dependencies.
import fs from 'node:fs';import os from 'node:os';import path from 'node:path';import {spawn} from 'node:child_process';
export const chromeExecutable = () => process.env.CHROME_BIN || (process.platform === 'darwin' ? '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome' : 'chromium');
export function devToolsPort(dir) {
 let data;
 try { data=fs.readFileSync(path.join(dir,'DevToolsActivePort'),'utf8'); }
 catch(e) { if(e.code==='ENOENT')return null;throw e; }
 const [port,endpoint]=data.split('\n');
 return /^[1-9]\d{0,4}$/.test(port)&&Number(port)<=65535&&/^\/devtools\/browser\/[^\s]+$/.test(endpoint??'')?Number(port):null;
}
export async function start(){const output=process.env.COWPLAY_UI_OUTPUT??fs.mkdtempSync(path.join(os.tmpdir(),'cowplay-ui-shots-'));fs.mkdirSync(output,{recursive:true});const dir=fs.mkdtempSync(path.join(os.tmpdir(),'cowplay-ui-chrome-'));const chrome=spawn(chromeExecutable(),['--headless=new','--remote-debugging-address=127.0.0.1','--remote-debugging-port=0','--user-data-dir='+dir,'--no-first-run','--no-default-browser-check','--disable-background-networking','about:blank'],{stdio:['ignore','ignore',fs.openSync(path.join(dir,'chrome.log'),'a')]});
let ws;const close=async()=>{ws?.close();if(chrome.exitCode===null&&chrome.signalCode===null){chrome.kill('SIGTERM');await new Promise(r=>chrome.once('exit',r));}fs.rmSync(dir,{recursive:true,force:true,maxRetries:3,retryDelay:100});};
try {
const wait=async(fn,name,timeout=8000)=>{const end=Date.now()+timeout;while(Date.now()<end){if(await fn())return;await new Promise(r=>setTimeout(r,50));}throw Error('timeout: '+name)};
let port;await wait(()=>{port=devToolsPort(dir);return port!==null;},'Chrome');const targets=await(await fetch(`http://127.0.0.1:${port}/json/list`)).json();ws=new WebSocket(targets.find(t=>t.type==='page').webSocketDebuggerUrl);await new Promise((r,j)=>{ws.onopen=r;ws.onerror=j});let id=0;const pending=new Map(),errors=[],warnings=[];
const call=(method,params={})=>new Promise((r,j)=>{pending.set(++id,m=>m.error?j(Error(JSON.stringify(m.error))):r(m.result));ws.send(JSON.stringify({id,method,params}))});ws.onmessage=e=>{const m=JSON.parse(e.data);if(m.method==='Runtime.exceptionThrown')errors.push(m.params.exceptionDetails.exception?.description??m.params.exceptionDetails.text);if(m.method==='Runtime.consoleAPICalled'&&m.params.type==='error')errors.push(m.params.args.map(a=>a.value??a.description??'').join(' '));if(m.method==='Runtime.consoleAPICalled'&&m.params.type==='warning')warnings.push(m.params.args.map(a=>a.value??a.description??'').join(' '));if(m.id){pending.get(m.id)?.(m);pending.delete(m.id)}};
await call('Page.enable');await call('Runtime.enable');const evaluate=async(expression)=>{const r=await call('Runtime.evaluate',{expression,returnByValue:true,awaitPromise:true,userGesture:true});if(r.exceptionDetails)throw Error(r.exceptionDetails.exception?.description??r.exceptionDetails.text);return r.result.value;};
return {call,evaluate,errors,warnings,wait,viewport:async(w,h)=>call('Emulation.setDeviceMetricsOverride',{width:w,height:h,deviceScaleFactor:1,mobile:false}),goto:async(url)=>{await call('Page.navigate',{url});await wait(()=>evaluate(`document.URL===${JSON.stringify(url)}&&document.readyState==='complete'`),'load')},shot:async(name)=>{const r=await call('Page.captureScreenshot',{format:'png',captureBeyondViewport:false});const p=path.join(output,name+'.png');fs.writeFileSync(p,Buffer.from(r.data,'base64'));return p},close};
}catch(e){const log=path.join(dir,'chrome.log');const detail=fs.existsSync(log)?fs.readFileSync(log,'utf8').slice(-4096):'';const exited=chrome.exitCode;await close();throw new Error(`${e.message}\nChrome exit: ${exited}\n${detail}`,{cause:e});}
}
