// Synthetic browser regression. Screenshots are written outside the repository.
import assert from 'node:assert/strict';import {confirmation} from './lib/confirmation-contract.mjs';import {start} from './lib/ui-browser.mjs';import {startFixture} from './lib/ui-fixture.mjs';const service=await startFixture(),{origin,fixture}=service;const b=await start(),shots=[],checks=[];const delay=ms=>new Promise(r=>setTimeout(r,ms));
try{
await b.call('Page.addScriptToEvaluateOnNewDocument',{source:`(()=>{window.testOnline=true;Object.defineProperty(navigator,'onLine',{configurable:true,get:()=>window.testOnline});window.wsTrace=[];const Native=window.WebSocket;window.WebSocket=class extends Native{constructor(...a){super(...a);this.addEventListener('open',()=>wsTrace.push('open'));this.addEventListener('close',e=>wsTrace.push(['close',e.code,e.reason]));this.addEventListener('error',()=>wsTrace.push('error'));this.addEventListener('message',e=>{if(wsTrace.length<20)wsTrace.push(typeof e.data==='string'?e.data:'binary')})}send(s){wsTrace.push(typeof s==='string'?s:'binary out');super.send(s)}};const pixels=new OffscreenCanvas(1208,720);const ctx=pixels.getContext('2d');ctx.fillStyle='#131313';ctx.fillRect(0,0,1208,720);ctx.fillStyle='#717171';ctx.textAlign='center';ctx.font='24px sans-serif';ctx.fillText('合成视频 · 仅用于界面验证',604,360);window.VideoDecoder=class {state='unconfigured';decodeQueueSize=0;constructor(init){this.init=init}configure(){this.state='configured'}decode(c){if(window.blockDecode)return;setTimeout(()=>{if(this.state==='configured')this.init.output(new VideoFrame(pixels,{timestamp:c.timestamp}))},window.paintDelay??0)}close(){this.state='closed'}static async isConfigSupported(c){return {supported:true,config:c}}};})();`});
await fixture({mode:'session',auth:true,phase:'closed',open:false,code:null,clear:true,paused:false});await fetch(origin+'/api/prefs',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({prefs:{debug:false,rail_side:'left'}})});await b.viewport(1280,720);await b.goto(origin+'/car');await b.wait(()=>b.evaluate("document.body.dataset.view==='session'"),'synthetic first painted frame');
const click=async sel=>{await b.evaluate(`document.querySelector(${JSON.stringify(sel)}).click()`);await delay(100)};
const shot=async name=>shots.push(await b.shot(name));
// Offline browsing stays quiet; only an attempted vehicle command raises a notice.
const ultra={enabled:true,channels:[true,true],climate_control:{available:false,allowed:false}};
await fixture({ultra,vehicle:{enabled:false,connected:false,phase:'unconfigured'}});
let reads=(await fixture({})).stateReads;
await b.wait(async()=>(await fixture({})).stateReads>reads,'offline state poll');
assert.equal(await b.evaluate("document.querySelector('#climate-status').hidden"),true,'offline browsing does not show a permanent warning');
const feedback={sequence:1,at_ms:1,age_ms:0,message:'车辆未连接，操作未发送；当前为温控预设'};
await fixture({ultra:{...ultra,climate_control:{...ultra.climate_control,feedback}}});
await b.wait(()=>b.evaluate("!document.querySelector('#climate-status').hidden"),'offline command feedback');
for(const [w,h] of [[1280,720],[390,844]]){
 await b.viewport(w,h);await delay(100);
 assert.ok(await b.evaluate("(()=>{const e=document.querySelector('#climate-status'),r=e.getBoundingClientRect();return r.width>0&&r.left>=0&&r.right<=innerWidth&&r.bottom<=innerHeight&&getComputedStyle(e).pointerEvents==='none'&&e.getAttribute('role')==='status'&&document.elementFromPoint(r.x+r.width/2,r.y+r.height/2)!==e})()"),'feedback fits and never captures CarPlay touches');
 await shot(`car-ultra-command-feedback-${w}`);
}
await b.viewport(1280,720);
await b.wait(()=>b.evaluate("document.querySelector('#climate-status').hidden"),'feedback dismisses automatically');
reads=(await fixture({})).stateReads;
await b.wait(async()=>(await fixture({})).stateReads>reads,'same feedback polled again');
assert.equal(await b.evaluate("document.querySelector('#climate-status').hidden"),true,'polling must not repeat old feedback');
await fixture({ultra:{...ultra,climate_control:{available:true,allowed:false,feedback:{...feedback,sequence:2,message:'温控未授权，请在车辆管理中允许本次会话'}}}});
await b.wait(()=>b.evaluate("!document.querySelector('#climate-status').hidden&&document.querySelector('#climate-status').textContent.includes('温控未授权')"),'new command permission feedback');
await fixture({ultra:{...ultra,channels:[false,false]}});
await b.wait(()=>b.evaluate("document.querySelector('#climate-status').hidden"),'released Ultra channels clear status');
await fixture({ultra:null,vehicle:null});checks.push('offline climate browsing stays quiet; attempted commands show transient touch-through feedback once');
await shot('car-session-1280');assert.equal(await b.evaluate("document.querySelector('#region').clientWidth"),1208);await click('#r-sheet');assert.equal(await b.evaluate("!document.querySelector('#sheet').hidden"),true);await shot('car-session-controls-1280');checks.push('control centre leaves video visible');
await click('#sheet-diagnostics-group > summary');await click('#sheet-debug [role=switch]');await b.wait(()=>b.evaluate("!document.querySelector('#stats').hidden"),'stats');await click('#sheet-done');await b.wait(()=>b.evaluate("document.querySelector('#stats-rows').textContent.includes('1250 MB')"),'fresh stats sample');await shot('car-session-stats-1280');assert.ok(await b.evaluate("['画面 / 声明','本页绘制 / 丢弃','解码队列','浏览器 ↔ 接收端','采样年龄','页面 / 服务构建'].every(k=>[...document.querySelectorAll('#stats-rows .k')].some(e=>e.textContent===k))"));checks.push('debug rows distinguish actual frames, declaration, sample age and builds');
await b.viewport(800,480);await delay(250);assert.equal(await b.evaluate("document.querySelector('#region').clientWidth"),736);assert.equal(await b.evaluate("document.querySelectorAll('#stats-rows .row').length"),9);assert.ok(await b.evaluate("document.querySelector('#stats').getBoundingClientRect().height<240&&[...document.querySelectorAll('#stats-rows .row')].every(e=>e.getBoundingClientRect().height<=20)"),'compact statistics stay in a short overlay rather than inheriting settings row heights');await shot('car-session-stats-800');checks.push('compact 736x480 region and 9 debug rows');
await b.viewport(1920,720);await delay(250);assert.equal(await b.evaluate("document.querySelector('#region').clientWidth"),1848);await click('#r-sheet');await shot('car-session-controls-1920');checks.push('wide 1848x720 region');
await click('#sheet-done');await click('#r-full');assert.equal(await b.evaluate("!document.querySelector('#handle').hidden"),true);await shot('car-session-full');await click('#handle');assert.equal(await b.evaluate("document.querySelector('#handle').hidden"),true);checks.push('fullscreen and handle exit');
// A healthy static source can stop sending frames while pongs continue.
await fixture({paused:true});const quietAt=await b.evaluate('wsTrace.length');await delay(6600);
assert.equal(await b.evaluate("document.querySelector('#stall').hidden"),true,'static picture must not be marked stalled');
assert.equal(await b.evaluate(`wsTrace.slice(${quietAt}).some(x=>typeof x==='string'&&/^\\{/.test(x)&&['keyframe','stall'].includes(JSON.parse(x).type))`),false,'idle must not create recovery traffic');
checks.push('static picture does not trigger recovery or stall accounting');
// A normal first frame after a long idle must receive its own rendering budget.
await b.evaluate('window.paintDelay=150');await fixture({paused:false});await delay(800);await fixture({paused:true});await delay(300);
assert.equal(await b.evaluate("document.querySelector('#stall').hidden"),true,'healthy frame after idle must not inherit the idle age');
await b.evaluate('window.paintDelay=0');checks.push('healthy delayed frame after long idle is not a stall');
// Continue input while deliberately preventing decoder output: this is observable blockage.
await b.evaluate('window.blockDecode=true');await fixture({paused:false});
await b.wait(()=>b.evaluate("!document.querySelector('#stall').hidden"),'incoming frames without drawing produce recovery');await shot('car-session-stall');
await b.evaluate('window.blockDecode=false');await b.wait(()=>b.evaluate("document.querySelector('#stall').hidden"),'drawing resumes after recovery');
checks.push('incoming frames with blocked decoder trigger recovery and then clear');
await click('#r-sheet');await click('#sheet-stop');assert.equal((await fixture({})).cmds.filter(x=>x.cmd==='disconnect').length,0);await confirmation(b);await b.wait(()=>b.evaluate("document.body.dataset.view==='connect'"),'stopped');assert.equal((await fixture({})).cmds.filter(x=>x.cmd==='disconnect').length,1);checks.push('explicit confirmation before full disconnect');
// Adding an iPhone from the stopped home reopens the video channel: the new phone's picture appears with no extra tap.
{const ws0=(await fixture({})).wsAttempts;
 await b.wait(()=>b.evaluate("!!document.querySelector('#c-body .choice.add')"),'add row on the stopped home');await b.evaluate("document.querySelector('#c-body .choice.add').click()");await confirmation(b);
 await b.wait(async()=>(await fixture({})).cmds.some(x=>x.cmd==='openPairWindow'),'add from the stopped home');
 await b.wait(async()=>(await fixture({})).wsAttempts>ws0,'adding reopens the video channel');await b.wait(()=>b.evaluate("document.body.dataset.scene==='window'"),'add scene');
 await fixture({phase:'closed',open:false,mode:'session'});await b.wait(()=>b.evaluate("document.body.dataset.view==='session'"),'picture after adding from the stopped home');
 checks.push('adding a phone after a disconnect reaches the picture without another tap');
 await fixture({stateFail:true});await click('#r-sheet');await click('#sheet-stop');await confirmation(b);await b.wait(()=>b.evaluate("document.body.dataset.scene==='stopped'"),'stopped again');}
// A stop lifted on another screen reopens this channel but never takes that screen's picture automatically.
{const ws1=(await fixture({})).wsAttempts;await fixture({stateFail:false,stopped:false,mode:'takeover'});
 await b.wait(()=>b.evaluate("document.body.dataset.scene==='takeover'"),'stop lifted elsewhere shows the other screen');
 await delay(3500);assert.equal((await fixture({})).wsAttempts,ws1+1,'no automatic takeover after a stop lifted elsewhere');
 checks.push('a stop lifted elsewhere never takes another screen automatically');
 await fixture({mode:'session'});await b.evaluate("[...document.querySelectorAll('#c-body button')].find(e=>e.textContent.startsWith('在这块屏幕显示')).click()");await confirmation(b);
 await b.wait(()=>b.evaluate("document.body.dataset.view==='session'"),'manual takeover paints');
 await click('#r-sheet');await click('#sheet-stop');await confirmation(b);await b.wait(()=>b.evaluate("document.body.dataset.scene==='stopped'"),'stopped for the next check');}

// A prior manual takeover must not leak into quiet-resume retries after a transport failure.
{const ws0=(await fixture({})).wsAttempts,traceAt=await b.evaluate('wsTrace.length');
 await fixture({mode:'takeover',stopped:false,rejectWS:true});
 await b.wait(()=>b.evaluate(`wsTrace.slice(${traceAt}).some(e=>Array.isArray(e)&&e[0]==='close')`),'quiet resume transport failure');
 await fixture({rejectWS:false});
 await b.wait(()=>b.evaluate("document.body.dataset.scene==='takeover'"),'quiet retry respects the other screen');
 const hellos=await b.evaluate(`wsTrace.slice(${traceAt}).filter(e=>typeof e==='string'&&e.startsWith('{')).map(e=>JSON.parse(e)).filter(e=>typeof e.takeover==='boolean'&&e.window)`);
 assert.ok(hellos.length>=2);assert.ok(hellos.every(e=>e.takeover===false),'quiet-resume retries never inherit prior manual takeover');
 await delay(3500);assert.equal((await fixture({})).wsAttempts,ws0+2,'failed connection and one quiet retry only');
 await fixture({mode:'session'});await b.evaluate("[...document.querySelectorAll('#c-body button')].find(e=>e.textContent.startsWith('在这块屏幕显示')).click()");await confirmation(b);
 await b.wait(()=>b.evaluate("document.body.dataset.view==='session'"),'explicit takeover after quiet retry');
 await click('#r-sheet');await click('#sheet-stop');await confirmation(b);await b.wait(()=>b.evaluate("document.body.dataset.scene==='stopped'"),'stopped after quiet retry regression');
 checks.push('quiet-resume retries after transport failure do not inherit a previous manual takeover');}

// An offline hint preserves a reachable local channel and continuing painted frames.
await fixture({mode:'session'});await b.wait(()=>b.evaluate("!!document.querySelector('#c-body .choice[aria-checked=true]')"),'stopped scene lists saved phones');await b.evaluate("document.querySelector('#c-body .choice[aria-checked=true]').click()");await confirmation(b);await b.wait(()=>b.evaluate("document.body.dataset.view==='session'"),'manual connect after stop');
const offlineCloses=await b.evaluate("wsTrace.filter(e=>Array.isArray(e)&&e[0]==='close').length");await b.evaluate("window.testOnline=false;window.dispatchEvent(new Event('offline'))");const offlineAttempts=(await fixture({})).wsAttempts;await delay(1100);assert.equal((await fixture({})).wsAttempts,offlineAttempts);assert.equal(await b.evaluate("wsTrace.filter(e=>Array.isArray(e)&&e[0]==='close').length"),offlineCloses);
await b.evaluate("window.testOnline=true;window.dispatchEvent(new Event('online'))");await b.wait(()=>b.evaluate("document.querySelector('#cover').hidden&&document.querySelector('#freeze').hidden&&document.body.dataset.view==='session'&&!document.body.classList.contains('stalled')"),'reachable local channel stays painted');checks.push('offline/online hints preserve a reachable local channel without closing or reopening it');
// Initial try plus four exponential retries; handshake alone must not renew the budget.
// End the previous page before resetting the shared fixture counter; its pending
// viewer must not be counted as this fresh page's first try.
await b.goto('about:blank');
await fixture({mode:'connecting',rejectWS:true,resetAttempts:true});await b.goto(origin+'/car?retry-budget');
await b.wait(()=>b.evaluate("document.querySelector('#c-lead').textContent.includes('自动重连 4 次')"),'four bounded retries',20000);assert.equal((await fixture({})).wsAttempts,5);
await delay(1100);assert.equal((await fixture({})).wsAttempts,5);checks.push('exactly four automatic retries, then explicit recovery');
await fixture({mode:'session',rejectWS:false});await b.evaluate("[...document.querySelectorAll('#c-body button')].find(e=>e.textContent==='重新连接').click()");await confirmation(b);await b.wait(()=>b.evaluate("document.body.dataset.view==='session'"),'manual recovery after retry exhaustion');checks.push('manual reconnect paints after exhausted budget');
// Returning from a suspended page has no guarantee of an `online` event.
await b.goto('about:blank');
await fixture({mode:'connecting',rejectWS:true,resetAttempts:true});await b.goto(origin+'/car?standby-return');
await b.wait(()=>b.evaluate("document.querySelector('#c-lead').textContent.includes('自动重连 4 次')"),'standby return starts with an exhausted retry budget',20000);
assert.equal((await fixture({})).wsAttempts,5);
await fixture({mode:'session',rejectWS:false});
await b.evaluate("window.dispatchEvent(new PageTransitionEvent('pageshow',{persisted:true}));document.dispatchEvent(new Event('visibilitychange'))");
await b.wait(()=>b.evaluate("document.body.dataset.view==='session'&&document.querySelector('#cover').hidden&&document.querySelector('#freeze').hidden"),'page return paints without online or manual connect');
assert.equal((await fixture({})).wsAttempts,6,'pageshow and visibility recovery share one socket');
checks.push('page return resets an exhausted channel budget once and paints without online');
await click('#r-sheet');await click('#sheet-stop');await confirmation(b);
await b.wait(()=>b.evaluate("document.body.dataset.scene==='stopped'"),'manual stop before page return');
const stoppedAttempts=(await fixture({})).wsAttempts,stoppedCommands=(await fixture({})).cmds.length;
await b.evaluate("window.dispatchEvent(new PageTransitionEvent('pageshow',{persisted:true}));document.dispatchEvent(new Event('visibilitychange'));window.dispatchEvent(new Event('online'))");
await delay(2200);
assert.equal((await fixture({})).wsAttempts,stoppedAttempts,'page return cannot reopen a manually stopped channel');
assert.equal((await fixture({})).cmds.length,stoppedCommands,'page return cannot call a manually disconnected phone');
checks.push('manual stop survives pageshow, visibility and online events');
await fixture({mode:'session',stopped:false});await b.wait(()=>b.evaluate("document.body.dataset.view==='session'"),'explicit stop lifted by fixture for remaining authorization tests');
// Repeated authorization loss returns to one keypad, without duplicate key handlers.
for(let i=0;i<2;i++){
 await fixture({auth:false});await b.wait(()=>b.evaluate("!document.querySelector('#token-layer').hidden"),'revoked authorization keypad');assert.equal(await b.evaluate("document.querySelectorAll('#keys button').length"),12);
 await b.evaluate("for(const k of '123456')[...document.querySelectorAll('#keys button')].find(e=>e.textContent===k).click()");await b.wait(()=>b.evaluate("document.body.dataset.view==='session'"),'authorized again');
}
checks.push('repeated auth loss returns to a single working keypad');
// Pairing a new phone ends the stream; the car leaves the frozen picture for the home add scene.
await fixture({phase:'Waiting',open:true,nearby:[],sessionReset:true,mode:'connecting'});await b.wait(()=>b.evaluate("document.body.dataset.view==='connect'&&document.body.dataset.scene==='window'"),'stream ended by pairing returns to the home');await shot('car-session-to-pairing');checks.push('a pairing window after the stream ends shows the home add scene');
assert.deepEqual(b.errors,[]);console.log(JSON.stringify({checks,shots,errors:b.errors,warnings:b.warnings},null,2));
}catch(e){console.log(JSON.stringify({error:String(e),errors:b.errors,text:await b.evaluate('({text:document.body.innerText,trace:window.wsTrace})'),shot:await b.shot('session-failure')}));throw e;}finally{await b.close();await service.close()}
