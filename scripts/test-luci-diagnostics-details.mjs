import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import {start} from './lib/ui-browser.mjs';
import {startFixture} from './lib/ui-fixture.mjs';
import {startLuciFixture} from './lib/luci-fixture.mjs';
const api=await startFixture(),luci=await startLuciFixture(process.env.LUCI_RUNTIME_DIR,api.origin,{deviceAppend:process.env.LUCI_DEVICE_APPEND==='1'}),b=await start();
const checks=[],output=fs.mkdtempSync(path.join(os.tmpdir(),'cowplay-public-certificate-test-'));
const click=async text=>b.evaluate(`(()=>{const e=[...document.querySelectorAll('button,[role=tab]')].find(e=>e.offsetParent!==null&&e.textContent.trim()===${JSON.stringify(text)});if(!e||e.disabled)throw Error('unavailable: '+${JSON.stringify(text)});e.click();})()`);
const settled=()=>b.wait(()=>b.evaluate("(async()=>{const v=await L.require('view.cowplay.main');return !v.diagPending})()"),'diagnostic view settled');
const panel=()=>b.evaluate("document.querySelector('[data-page=diag]').textContent");
try {
 await api.fixture({mode:'session',auth:true});await b.viewport(1440,1000);await b.goto(luci.origin.replace('127.0.0.1','localhost.')+'/luci#checks');
 await b.wait(()=>b.evaluate("!!document.querySelector('.cw-measurements')"),'initial snapshot without clicking start');
 assert.ok((await panel()).includes('采样中'));
 await click('停止采样');await settled();
 const callsBefore=await fetch(luci.origin+'/__calls').then(r=>r.json());
 await b.evaluate("(async()=>{const v=await L.require('view.cowplay.main');await v.refresh();if(v.diagPending)await v.diagPending.promise})()");
 const callsAfter=await fetch(luci.origin+'/__calls').then(r=>r.json());
 assert.equal(callsAfter.filter(c=>c.method==='diag').length,callsBefore.filter(c=>c.method==='diag').length);
 checks.push('real-time view starts sampling, and explicit pause stops diagnostic requests');
 for(const legacyMetricGroups of [false,true]) {
  await fetch(luci.origin+'/__fixture',{method:'POST',body:JSON.stringify({legacyMetricGroups,wifiStatus:'driver_unavailable',backgroundViewer:true})});
  await b.evaluate("(async()=>{const v=await L.require('view.cowplay.main');v.sampling=true;await v.loadDiag()})()");
  const measured=await b.evaluate("(()=>{const groups=[...document.querySelectorAll('.cw-measurements h4')].map(e=>e.textContent),browser=document.querySelector('.cw-measurements section');return {groups,rtt:browser.querySelector('.cw-reading').textContent,background:browser.textContent.includes('投屏页在后台'),age:browser.textContent.includes('实时上报')}})()");
  assert.deepEqual(measured,{groups:['车机 ⟷ 接收端','画面','接收端'],rtt:'12 ms',background:true,age:true},legacyMetricGroups?'legacy group compatibility':'receiver group behavior');
 }
 await fetch(luci.origin+'/__fixture',{method:'POST',body:JSON.stringify({legacyMetricGroups:false,wifiStatus:null,backgroundViewer:false})});
 await click('停止采样');await settled();
 checks.push('new and legacy metric groups preserve background RTT, sample age and omission of unmeasured phone wireless values');
 await b.evaluate("(async()=>{const cp=await L.require('cowplay.rpc'),old=cp.call.bind(cp);cp.call=async(m,...args)=>{const r=await old(m,...args);if(m==='diag'){r.stats.car_sample_age_ms=20;r.stats.server.fps_in=0;r.stats.car={...r.stats.car,fps:0,decode_ms:6.2};r.diag.checks=r.diag.checks.map(c=>c.group==='画面'&&c.name!=='停住'?{...c,known:false,value:null}:c);}return r;};})()");
 await click('开始采样');await b.wait(()=>b.evaluate("[...document.querySelectorAll('.cw-reading')].some(e=>e.textContent==='0.0 fps')"),'static measured zero');
 assert.ok((await panel()).includes('6.2 ms'));
 await click('停止采样');checks.push('static decoded picture keeps measured zero fps and last decode separate from unknown');
 await fetch(luci.origin+'/__fixture',{method:'POST',body:JSON.stringify({statsPatch:{car:null,car_last_report:{fps:0,rtt_ms:9.3,decode_ms:6.2,decoded:42,width:1920,height:912,visible:false},car_sample_age_ms:65000},statusPatch:{sessions:[{id:'old-model',model:'iPhone17,1',started_ms:Date.now()-60000,ended_ms:Date.now()-1000,first_frame_ms:Date.now()-58000,end_reason:'协议或传输错误'},{id:'old-failure',started_ms:Date.now()-50000,ended_ms:Date.now()-49000,end_reason:'蓝牙连接失败',events:[{kind:'error',msg:'连接失败'}]},{id:'empty',started_ms:Date.now()-40000,ended_ms:Date.now()-39000,events:[]}]}})});
 await click('概览');await b.evaluate("(async()=>{const v=await L.require('view.cowplay.main');await v.refresh()})()");
 await b.wait(()=>b.evaluate("document.querySelector('[data-page=overview]').textContent.includes('65 秒前')"),'last report age');
 const overview=await b.evaluate("document.querySelector('[data-page=overview]').textContent");
 for(const t of ['0.0 fps','9 ms','iPhone17,1','蓝牙连接失败','协议或传输错误'])assert.ok(overview.includes(t),t);
 assert.equal(await b.evaluate("document.querySelector('[data-page=overview] table tbody').rows.length"),2);
 assert.ok(!overview.includes('车机屏幕iPhone请求画面'));
 checks.push('overview retains measured zero fps and RTT with explicit stale age; ended model and failed attempts remain useful while empty shells are hidden');
 await fetch(luci.origin+'/__fixture',{method:'POST',body:JSON.stringify({statsPatch:{car:null,car_last_report:{fps:0,rtt_ms:null,ping:{rtt_ms:12.5,age_ms:60000}},car_sample_age_ms:5000}})});
 await b.evaluate("(async()=>{const v=await L.require('view.cowplay.main');await v.refresh()})()");
 assert.ok(await b.evaluate("document.querySelector('[data-page=overview]').textContent.includes('13 ms') && document.querySelector('[data-page=overview]').textContent.includes('最近探测 · 65 秒前')"));
 checks.push('a missing rolling average retains the real last pong with its own sample age');
 const originalDiag=await fetch(api.origin+'/api/diag').then(r=>r.json());
 await api.fixture({diagPatch:{checks:originalDiag.checks.map(c=>c.name==='延迟'?{...c,known:false,value:null}:c)}});
 await fetch(luci.origin+'/__fixture',{method:'POST',body:JSON.stringify({statsPatch:{car:{rtt_ms:null,ping:{rtt_ms:12.5,age_ms:1000},visible:true},car_last_report:null,car_sample_age_ms:20}})});
 await click('实时数据');await click('开始采样');
 await b.wait(()=>b.evaluate("document.querySelector('[data-page=diag] .cw-reading').textContent.startsWith('13 ms')"),'fresh visible last pong fallback');
 await click('停止采样');await api.fixture({diagPatch:originalDiag});await click('概览');
 checks.push('fresh visible reports with no rolling average show the measured pong on the real-time page');
 await fetch(luci.origin+'/__fixture',{method:'POST',body:JSON.stringify({statsPatch:{car:null,car_last_report:null,car_sample_age_ms:null},statusPatch:{context:{session:'new',viewer:'new',epoch:1,viewer_generation:2}}})});
 await b.evaluate("(async()=>{const v=await L.require('view.cowplay.main');await v.refresh()})()");
 assert.ok(!await b.evaluate("/9 ms|13 ms/.test(document.querySelector('[data-page=overview]').textContent)"));
 checks.push('a new channel without a report cannot reuse the previous RTT');
 await fetch(luci.origin+'/__fixture',{method:'POST',body:JSON.stringify({statsPatch:{},statusPatch:{},wifiStatus:'driver_unavailable'})});
 await b.evaluate("(async()=>{const v=await L.require('view.cowplay.main');await v.refresh()})()");
 await click('设备信息');await b.wait(()=>b.evaluate("document.querySelector('[data-page=diag]').textContent.includes('CarPlay Wi-Fi 热点')"),'wifi information');
 for(const text of ['实测接口','wlan1','配置 SSID','实测信道 / 频宽','80 MHz','客户端数','采样年龄','热点关联'])assert.ok((await panel()).includes(text),text);
 assert.ok(!(await panel()).includes('信号 / 速率 / 重传'));
 checks.push('Wi-Fi interface, observed/configured data, bandwidth, age and phone association source are distinct');
 await click('进程与配置');await b.wait(()=>b.evaluate("document.querySelector('[data-page=diag]').textContent.includes('端口与用途')"),'separate ports');
 assert.equal(await b.evaluate("document.querySelector('[data-page=diag] .cw-table tbody').rows.length"),3);
 assert.ok((await panel()).includes('动态媒体端口未逐个采集'));
 checks.push('fixed endpoints are three separate rows and do not claim to enumerate dynamic sockets');
 await click('日志');await b.wait(()=>b.evaluate("document.querySelector('[data-page=diag]').textContent.includes('RAM 环形日志')"),'log policy');
 for(const text of ['最近 150 条','4 MiB','32 MiB','128 MiB','7 天','4096','已淘汰 3 条','syslog'])assert.ok((await panel()).includes(text),text);
 assert.equal(await b.evaluate("document.querySelector('[data-page=diag] details').open"),false);
 const logStyle=await b.evaluate("(()=>{const p=document.querySelector('[data-scroll=logs]'),s=getComputedStyle(p);return {overflow:s.overflowY,maxHeight:s.maxHeight,lines:p.textContent.split('\\n').length}})()");
 assert.equal(logStyle.overflow,'auto');assert.notEqual(logStyle.maxHeight,'none');assert.equal(logStyle.lines,2);
 await b.evaluate("(async()=>{const v=await L.require('view.cowplay.main');await v.refresh();if(v.diagPending)await v.diagPending.promise})()");
 assert.equal(await b.evaluate("document.querySelector('[data-scroll=logs]').textContent.split('\\n').length"),2);
 checks.push('log view has a bounded scrolling area, replaces polls, and explains memory/rotation/retention/export scope');
 await click('网络与证书');
 await b.wait(()=>b.evaluate("document.querySelector('[data-page=network]').textContent.includes('Synthetic CarPlay Accessory')"),'current public certificate metadata');
 const certificateText=await b.evaluate("document.querySelector('[data-page=network]').textContent");
 for(const text of ['主体','签发者','生效时间','截止时间','Synthetic Test CA','Synthetic CarPlay Accessory'])assert.ok(certificateText.includes(text),text);
 assert.ok(!/序列号|SHA-256 指纹|01:02:03/.test(certificateText));
 assert.equal(await b.evaluate('location.hostname'), 'localhost.');
 await b.call('Browser.setDownloadBehavior',{behavior:'allow',downloadPath:output});
 for(const [label,file,kind] of [['下载当前 TLS 公共证书','cowplay-tls-public.pem','tls'],['下载当前 CarPlay 公共证书','cowplay-carplay-public.pem','carplay']]){
  await click(label);await b.wait(()=>fs.existsSync(path.join(output,file)),file);
  const pem=fs.readFileSync(path.join(output,file),'utf8');assert.ok(pem.includes('SYNTHETIC-PUBLIC'));assert.ok(!pem.includes('PRIVATE KEY'));
  const calls=await fetch(luci.origin+'/__calls').then(r=>r.json());assert.ok(calls.some(c=>c.method==='certificate_export'&&c.args.kind===kind));
 }
 for(const [label,file,kind] of [['下载当前 TLS 私钥','cowplay-tls-private.pem','tls'],['下载当前 CarPlay 私钥','cowplay-carplay-private.pk8','carplay']]){
  await click(label);await b.wait(()=>b.evaluate("!!document.querySelector('[role=dialog]')"),'key export confirmation');await click('取消');
  assert.ok(!fs.existsSync(path.join(output,file)));
  await click(label);await click('下载私钥');await b.wait(()=>fs.existsSync(path.join(output,file)),file);
  assert.equal(fs.readFileSync(path.join(output,file),'utf8'),'SYNTHETIC-PRIVATE-KEY-'+kind);
 }
 const keyCalls=await fetch(luci.origin+'/__calls').then(r=>r.json());
 assert.equal(keyCalls.filter(c=>c.object==='private-key-cgi').length,2);
 assert.ok(keyCalls.filter(c=>c.object==='private-key-cgi').every(c=>c.cookiePresent===false),'dedicated CGI authenticates the POST session even when LuCI cookie Path excludes it');
 assert.ok(!keyCalls.some(c=>c.method==='key_export'));
 checks.push('private keys require explicit confirmation and use only the dedicated download path, never normal polling/RPC');
 checks.push('current TLS and CarPlay public metadata are visible and separate fixed export actions download certificate-only fixture content');
 await b.shot('certificates-current-1440');
 for(const width of [1440,390]){
  await b.viewport(width,1000);
  for(const title of ['网络与证书','设备信息','进程与配置','日志','实时数据']){
   await click(title);if(title!=='网络与证书')await settled();
   assert.ok(await b.evaluate('document.documentElement.scrollWidth<=innerWidth'),title+' width '+width);
   assert.ok(!/(^|\s)(null|undefined|NaN)(\s|$)/.test(await b.evaluate("document.querySelector('.cw').innerText")),title+' optional children');
   await b.shot('details-'+title+'-'+width);
  }
 }
 assert.deepEqual(b.errors,[]);console.log(JSON.stringify({checks,errors:b.errors},null,2));
} finally {await b.close();await luci.close();await api.close();fs.rmSync(output,{recursive:true,force:true});}
