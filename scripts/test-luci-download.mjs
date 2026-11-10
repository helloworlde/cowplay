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
try {
 await b.call('Browser.setDownloadBehavior',{behavior:'allow',downloadPath:output});
 await api.fixture({mode:'session',auth:true});await b.goto(luci.origin+'/luci#checks');
 await b.wait(()=>b.evaluate("[...document.querySelectorAll('button')].some(e=>e.textContent==='下载诊断包')"),'export button');
 // Real input dispatch gives the button the same gesture lifetime as a user click.
 await b.evaluate("(()=>{const e=[...document.querySelectorAll('button')].find(e=>e.textContent==='下载诊断包');e.scrollIntoView({behavior:'instant',block:'center'});})()");
 let box;
 await b.wait(async()=>{
  box=await b.evaluate("(()=>{const e=[...document.querySelectorAll('button')].find(e=>e.textContent==='下载诊断包');if(!e||e.disabled)return null;const r=e.getBoundingClientRect(),x=r.x+r.width/2,y=r.y+r.height/2;return document.elementFromPoint(x,y)?.closest('button')===e?{x,y}:null;})()");
  return !!box;
 },'download button receives pointer');
 await b.call('Input.dispatchMouseEvent',{type:'mousePressed',button:'left',clickCount:1,...box});
 await b.call('Input.dispatchMouseEvent',{type:'mouseReleased',button:'left',clickCount:1,...box});
 await b.wait(()=>fs.existsSync(path.join(output,'cowplay-diag.json')),'diagnostic file received');
 const content=JSON.parse(fs.readFileSync(path.join(output,'cowplay-diag.json'),'utf8'));
 assert.equal(content.kind,'cowplay-diagnostics');assert.equal(content.logs,'synthetic only');
 assert.deepEqual(b.errors,[]);
 console.log(JSON.stringify({downloaded:true,bytes:fs.statSync(path.join(output,'cowplay-diag.json')).size,kind:content.kind,errors:b.errors},null,2));
} catch(e) {
 console.log(JSON.stringify({downloaded:false,error:String(e),files:fs.readdirSync(output),errors:b.errors,text:await b.evaluate("document.body.innerText")},null,2));throw e;
} finally {await b.close();await luci.close();await api.close();fs.rmSync(output,{recursive:true,force:true});}
