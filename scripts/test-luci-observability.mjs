import {luciConfirmation} from './lib/luci-confirmation.mjs';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import {start} from './lib/ui-browser.mjs';
import {startFixture} from './lib/ui-fixture.mjs';
import {startLuciFixture} from './lib/luci-fixture.mjs';
const api=await startFixture(), luci=await startLuciFixture(process.env.LUCI_RUNTIME_DIR,api.origin), b=await start();
const checks=[],failures=[];
const test=async(name,run)=>{try{await run();checks.push(name);}catch(e){failures.push({name,error:String(e),state:await b.evaluate("(async()=>{const v=await L.require('view.cowplay.main');return {sampling:v.sampling,pending:v.diagPending?.key,text:v.diagBody.textContent}})()")});}};
const click=async text=>{await b.evaluate(`(()=>{const e=[...document.querySelectorAll('button,a')].find(e=>e.offsetParent!==null&&e.textContent.trim()===${JSON.stringify(text)});if(!e||e.disabled)throw Error('button unavailable: '+${JSON.stringify(text)});e.click();})()`);};
try {
 await api.fixture({mode:'session',auth:true});await b.viewport(1440,1000);await b.goto(luci.origin+'/luci#diag');
 await b.wait(()=>b.evaluate("!!document.querySelector('[data-diag]')"),'diagnostics');
 await test('one functional menu exposes every view and routes directly',async()=>{
  assert.equal(await b.evaluate("document.querySelectorAll('.cw [role=tablist]').length"),1);
  assert.deepEqual(await b.evaluate("[...document.querySelectorAll('.cw-tabs [role=tab]')].map(e=>e.textContent)"),['概览','设备与访问','投屏策略','网络与证书','实时数据','设备信息','时间线','日志','进程与配置','系统']);
  await click('日志');await b.wait(()=>b.evaluate("location.hash==='#logs'&&!!document.querySelector('[data-scroll=logs]')"),'direct log route');
  await click('实时数据');
 });
 if(process.env.COWPLAY_AURORA_CSS){
  const css=fs.readFileSync(process.env.COWPLAY_AURORA_CSS,'utf8');
  await b.evaluate(`(()=>{document.getElementById('luci-theme').remove();const s=document.createElement('style');s.textContent=${JSON.stringify(css)};document.head.append(s);})()`);
 }
 await test('sampling is explicit and returns actual sourced data',async()=>{
  await click('开始采样');await b.wait(()=>b.evaluate("document.querySelector('.cw-measurements')?.textContent.includes('12 ms')"),'sample result');
  assert.ok(await b.evaluate("document.querySelector('[data-page=diag]').textContent.includes('累计收到视频帧')"));
  assert.ok(await b.evaluate("document.querySelector('[data-page=diag]').textContent.includes('实际解码尺寸')"));
  assert.ok(await b.evaluate("[...document.querySelectorAll('.cw-reading')].some(e=>e.getBoundingClientRect().width>0&&e.getBoundingClientRect().height>0&&e.textContent==='12 ms')"),'sample values are actually visible');
  await b.shot('sampling-1440');
  await click('停止采样');
  const before=await fetch(luci.origin+'/__calls').then(r=>r.json());
  await b.evaluate("(async()=>{const v=await L.require('view.cowplay.main');await v.refresh();if(v.diagPending)await v.diagPending.promise;})()");
  const after=await fetch(luci.origin+'/__calls').then(r=>r.json());
  assert.equal(after.filter(c=>c.method==='diag').length,before.filter(c=>c.method==='diag').length,'stopped sampling does not issue more diagnostic reads');
  await fetch(luci.origin+'/__fixture',{method:'POST',body:JSON.stringify({emptyReplies:{diag:true}})});
  await click('开始采样');await b.wait(()=>b.evaluate("document.querySelector('[data-page=diag]').textContent.includes('暂时无法读取此视图')"),'invalid sampling response is visible');
  await click('停止采样');await fetch(luci.origin+'/__fixture',{method:'POST',body:JSON.stringify({emptyReplies:{}})});
  await click('开始采样');await b.wait(()=>b.evaluate("(async()=>{const v=await L.require('view.cowplay.main');return !v.diagPending&&v.sampling&&document.querySelector('[data-page=diag] .cw-reading')?.textContent==='12 ms'})()"),'sampling recovers after invalid response');
  await click('停止采样');
 });
 await test('background viewer keeps actual RTT visible with its source limitation',async()=>{
  await fetch(luci.origin+'/__fixture',{method:'POST',body:JSON.stringify({backgroundViewer:true})});
  await click('开始采样');await b.wait(()=>b.evaluate("document.querySelector('[data-page=diag]').textContent.includes('投屏页在后台')"),'background source');
  assert.equal(await b.evaluate("document.querySelector('[data-page=diag] .cw-reading').textContent"),'12 ms');
  await click('停止采样');await fetch(luci.origin+'/__fixture',{method:'POST',body:JSON.stringify({backgroundViewer:false})});
 });
 await test('read operations never leave duplicate global success banners',async()=>{
  for(let i=1;i<=2;i++){
   await click('开始检查');
   await b.wait(async()=>(await fetch(luci.origin+'/__calls').then(r=>r.json())).filter(c=>c.method==='selftest').length>=i,'check request');
   await b.wait(()=>b.evaluate("(async()=>{const v=await L.require('view.cowplay.main');return !!v.segmentResult&&!v.diagPending;})()"),'check rendered');
  }
  assert.equal(await b.evaluate("document.body.textContent.includes('操作已完成')"),false,'no persistent generic success banner');
 });
 await test('configuration scroll survives a real poll update',async()=>{
  await click('进程与配置');await b.wait(()=>b.evaluate("!!document.querySelector('[data-diag=proc][aria-selected=true]')"),'proc tab');
  await b.evaluate("(async()=>{const v=await L.require('view.cowplay.main');if(v.diagPending)await v.diagPending.promise;const cp=await L.require('cowplay.rpc'),old=cp.call.bind(cp);cp.call=(m,...a)=>old(m,...a).then(r=>m==='proc'?{...r,config:Array.from({length:200},(_,i)=>' option synthetic_'+i+' test').join('\\n')}:r);await v.loadDiag();document.querySelector('[data-page=diag] pre').scrollTop=900;})()");
  assert.equal(await b.evaluate("document.querySelector('[data-page=diag] pre').scrollTop"),900,'reproduction starts with a scrolled long config');
  assert.equal(await b.evaluate("(async()=>{await(await L.require('view.cowplay.main')).refresh();const v=await L.require('view.cowplay.main');if(v.diagPending)await v.diagPending.promise;return document.querySelector('[data-page=diag] pre').scrollTop})()"),900);
 });
 await test('logs and timeline show raw text with scroll-to-bottom',async()=>{
  await click('日志');await b.wait(()=>b.evaluate("!!document.querySelector('select[aria-label=\"日志级别\"]')"),'logs');
  assert.ok(await b.evaluate("!!document.querySelector('[data-page=diag] pre')&&document.querySelector('[data-page=diag] pre').textContent.includes('WARN bt')"));
  const raw=await b.evaluate("(()=>{const e=document.querySelector('[data-scroll=logs]'),s=getComputedStyle(e);return {text:e.textContent,font:parseFloat(s.fontSize),line:parseFloat(s.lineHeight)};})()");
  assert.equal(raw.text.includes('\n\n'),false,'each log entry has one line ending');
  assert.equal(raw.text.split('\n').length,2,'two original entries occupy two lines');
  assert.ok(raw.font<=13&&raw.line/raw.font<=1.4,'log typography stays compact across themes');
  await click('滚动到底部');await click('时间线');await b.wait(()=>b.evaluate("[...document.querySelectorAll('button')].some(e=>e.textContent==='全部事件')"),'timeline loaded');await click('全部事件');
  await b.wait(()=>b.evaluate("document.querySelector('[data-page=diag] pre')?.textContent.includes('合成连接测试')"),'raw timeline');
 });
 await test('MFi identity has separate binary import fields',async()=>{
  await click('网络与证书');assert.ok(await b.evaluate("!!document.querySelector('input[aria-label=\"CarPlay 证书 P7B / DER\"]')&&!!document.querySelector('input[aria-label=\"CarPlay 私钥 PKCS8 DER\"]')"));
 });
 await test('MFi binary import confirms, clears secrets and never restarts the device',async()=>{
  const selectFile=(label,bytes)=>b.evaluate(`(()=>{const e=document.querySelector('input[aria-label='+${JSON.stringify(JSON.stringify(label))}+']'),d=new DataTransfer();d.items.add(new File([new Uint8Array(${JSON.stringify(bytes)})],'synthetic.der'));e.files=d.files;e.dispatchEvent(new Event('change'));})()`);
  const enabled=()=>b.evaluate("[...document.querySelectorAll('button')].some(e=>e.textContent==='验证并导入 CarPlay 身份'&&!e.disabled)");
  await selectFile('CarPlay 证书 P7B / DER',[0x30,0x01,0x00]);await selectFile('CarPlay 私钥 PKCS8 DER',[0x30,0x01,0x01]);
  await b.wait(enabled,'binary pair read');
  await selectFile('CarPlay 私钥 PKCS8 DER',[0]);await b.wait(()=>b.evaluate("document.querySelector('[data-page=network]').textContent.includes('请选择二进制 DER')"),'invalid file');assert.equal(await enabled(),false);
  await selectFile('CarPlay 私钥 PKCS8 DER',[0x30,0x01,0x01]);await b.wait(enabled,'replacement read');
  await click('验证并导入 CarPlay 身份');
  assert.equal((await fetch(luci.origin+'/__calls').then(r=>r.json())).some(c=>c.method==='mfi_upload'),false,'first click only confirms');
  await luciConfirmation(b);
  await b.wait(()=>b.evaluate("document.querySelector('[data-page=network]').textContent.includes('新身份已保存，等待手动重启')"),'pending restart shown');
  const calls=await fetch(luci.origin+'/__calls').then(r=>r.json()),upload=calls.find(c=>c.method==='mfi_upload');
  assert.match(upload.args.certificate,/BEGIN PKCS7/);assert.match(upload.args.private_key,/BEGIN PRIVATE KEY/);
  assert.equal(calls.some(c=>c.method==='service'),false);
  assert.ok(await b.evaluate("[...document.querySelectorAll('input[aria-label^=CarPlay]')].every(e=>e.value==='')"),'file inputs cleared after success');
  assert.equal(await enabled(),false);
 });
 await test('required restart stays visible after applying from another view',async()=>{
  await b.goto(luci.origin+'/luci#network');await b.wait(()=>b.evaluate("!!document.querySelector('.cw-tabs')"),'fresh config view');
  await fetch(luci.origin+'/__fixture',{method:'POST',body:JSON.stringify({reloadRequiresRestart:true})});
  await b.evaluate("(async()=>{const v=await L.require('view.cowplay.main'),ui=await L.require('ui');window.__ordinaryApply=ui.changes.apply;ui.changes.apply=()=>{};window.__applying=v.handleSaveApply();})()");await luciConfirmation(b);await b.evaluate("(async()=>{await window.__applying;const v=await L.require('view.cowplay.main'),ui=await L.require('ui');ui.changes.apply=window.__ordinaryApply;v.select('overview');document.dispatchEvent(new CustomEvent('uci-applied'));})()");
  await b.wait(()=>b.evaluate("!document.querySelector('.cw-footer').hidden&&document.querySelector('.cw-footer').textContent.includes('需要重启 CowPlay')"),'restart requirement remains visible on overview');
  await fetch(luci.origin+'/__fixture',{method:'POST',body:JSON.stringify({processPid:null})});
  await b.evaluate("(async()=>{await(await L.require('view.cowplay.main')).refresh();})()");
  assert.equal(await b.evaluate("document.querySelector('.cw-footer').hidden"),false,'unknown process does not imply a restart');
  await fetch(luci.origin+'/__fixture',{method:'POST',body:JSON.stringify({processPid:1234})});
  await b.evaluate("(async()=>{await(await L.require('view.cowplay.main')).refresh();})()");
  assert.equal(await b.evaluate("document.querySelector('.cw-footer').hidden"),false,'same process retains the requirement');
  await fetch(luci.origin+'/__fixture',{method:'POST',body:JSON.stringify({processPid:5678,reloadRequiresRestart:false})});
  await b.evaluate("(async()=>{await(await L.require('view.cowplay.main')).refresh();})()");
  assert.equal(await b.evaluate("document.querySelector('.cw-footer').hidden"),true,'new process clears the old requirement');
  await click('网络与证书');
 });
 await b.evaluate("(async()=>{const v=await L.require('view.cowplay.main');v.mfiSection.scrollIntoView({block:'start'});})()");await b.shot('mfi-import-1440');
 await b.viewport(390,844);assert.ok(await b.evaluate('document.documentElement.scrollWidth<=innerWidth'),'MFi page fits mobile');
 assert.equal(await b.evaluate("new Set([...document.querySelectorAll('.cw-tabs [role=tab]')].map(e=>Math.round(e.getBoundingClientRect().top))).size"),1,'all menu entries stay on one row at 390px');
 await b.evaluate("(async()=>{const v=await L.require('view.cowplay.main');v.mfiSection.scrollIntoView({block:'start'});})()");await b.shot('mfi-import-390');
 console.log(JSON.stringify({checks,failures,errors:b.errors,shot:await b.shot('luci-observability')},null,2));
 assert.deepEqual(failures,[]);assert.deepEqual(b.errors,[]);
}finally{await b.close();await luci.close();await api.close();}
