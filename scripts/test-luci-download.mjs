// Exercise a real browser download using synthetic data, never device credentials.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import {start} from './lib/ui-browser.mjs';
import {startFixture} from './lib/ui-fixture.mjs';
import {startLuciFixture} from './lib/luci-fixture.mjs';
const api=await startFixture(),luci=await startLuciFixture(process.env.LUCI_RUNTIME_DIR,api.origin),b=await start();
const output=fs.mkdtempSync(path.join(os.tmpdir(),'cowplay-synthetic-download-'));
let readonly;
try {
 await b.call('Browser.setDownloadBehavior',{behavior:'allow',downloadPath:output});
 await api.fixture({mode:'session',auth:true});await b.goto(luci.origin+'/luci#logs');
 await b.wait(()=>b.evaluate("[...document.querySelectorAll('button')].some(e=>e.textContent==='下载日志')"),'export button');
 // Real input dispatch gives the button the same gesture lifetime as a user click.
 await b.evaluate("(()=>{const e=[...document.querySelectorAll('button')].find(e=>e.textContent==='下载日志');e.scrollIntoView({behavior:'instant',block:'center'});})()");
 let box;
 await b.wait(async()=>{
  box=await b.evaluate("(()=>{const e=[...document.querySelectorAll('button')].find(e=>e.textContent==='下载日志');if(!e||e.disabled)return null;const r=e.getBoundingClientRect(),x=r.x+r.width/2,y=r.y+r.height/2;return document.elementFromPoint(x,y)?.closest('button')===e?{x,y}:null;})()");
  return !!box;
 },'download button receives pointer');
 await b.call('Input.dispatchMouseEvent',{type:'mousePressed',button:'left',clickCount:1,...box});
 await b.call('Input.dispatchMouseEvent',{type:'mouseReleased',button:'left',clickCount:1,...box});
 await b.wait(()=>fs.existsSync(path.join(output,'cowplay.log')),'diagnostic file received');
 const content=fs.readFileSync(path.join(output,'cowplay.log'),'utf8');
 for(const part of ['CowPlay 运行日志','WARN bt synthetic reconnect failed','ERROR video synthetic header timeout','较早日志已截断'])assert.ok(content.includes(part),part);
 assert.throws(()=>JSON.parse(content));
 const calls=await fetch(luci.origin+'/__calls').then(r=>r.json());
 assert.equal(calls.filter(c=>c.method==='logs_export').length,1);
 assert.equal(calls.some(c=>['bundle','selftest'].includes(c.method)),false);
 await fetch(luci.origin+'/__fixture',{method:'POST',body:JSON.stringify({emptyReplies:{logs_export:true}})});
 await b.evaluate("[...document.querySelectorAll('button')].find(e=>e.textContent==='下载日志').click()");
 await b.wait(()=>b.evaluate("document.body.textContent.includes('操作未完成')"),'invalid export is visible');
 assert.deepEqual(fs.readdirSync(output),['cowplay.log'],'an invalid reply must not become a downloaded file');
 readonly=await startLuciFixture(process.env.LUCI_RUNTIME_DIR,api.origin,{readOnly:true});
 await b.goto(readonly.origin+'/luci#logs');
 await b.wait(()=>b.evaluate("[...document.querySelectorAll('button')].some(e=>e.textContent==='下载日志'&&e.disabled)"),'read-only role cannot export administrator logs');
 assert.equal(await b.evaluate("(async()=>{try{await(await L.require('cowplay.rpc')).call('logs_export');return false;}catch{return true;}})()"),true);
 await b.evaluate("(async()=>{await(await L.require('view.cowplay.main')).select('network')})()");
 assert.equal(await b.evaluate("[...document.querySelectorAll('button')].filter(e=>/^下载当前 (TLS|CarPlay) 私钥$/.test(e.textContent)&&e.disabled).length"),2);
 assert.deepEqual(b.errors,[]);
 console.log(JSON.stringify({downloaded:true,bytes:fs.statSync(path.join(output,'cowplay.log')).size,format:'plain text',errors:b.errors},null,2));
} catch(e) {
 console.log(JSON.stringify({downloaded:false,error:String(e),files:fs.readdirSync(output),errors:b.errors,text:await b.evaluate("document.body.innerText")},null,2));throw e;
} finally {await b.close();await readonly?.close();await luci.close();await api.close();fs.rmSync(output,{recursive:true,force:true});}
