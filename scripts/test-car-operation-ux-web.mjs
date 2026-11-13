// Read-only browser review. Synthetic loopback API/media; no F50 or real iPhone.
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import {start} from './lib/ui-browser.mjs';
import {startFixture} from './lib/ui-fixture.mjs';
const results=[];
const source=`(()=>{const c=new OffscreenCanvas(1208,720);c.getContext('2d').fillRect(0,0,1208,720);window.reviewFrames=0;window.VideoDecoder=class{state='unconfigured';decodeQueueSize=0;constructor(i){this.i=i}configure(){this.state='configured'}decode(x){if(this.state==='configured'){this.i.output(new VideoFrame(c,{timestamp:x.timestamp}));window.reviewFrames++;}}close(){this.state='closed'}static async isConfigSupported(config){return {supported:true,config}}};const f=window.fetch;window.fetch=async(...args)=>{const r=await f(...args);const cmd=String(args[0]).endsWith('/api/cmd')?JSON.parse(args[1]?.body??'{}').cmd:null;if((cmd&&window.reviewHold===cmd)||window.reviewHoldPath===String(args[0]))return new Promise((resolve,reject)=>{const signal=args[1]?.signal;const abort=()=>reject(signal.reason);window.reviewRelease=()=>{signal?.removeEventListener('abort',abort);resolve(r)};if(signal?.aborted)abort();else signal?.addEventListener('abort',abort,{once:true});});return r;};})()`;
async function scenario(name,run){if(process.env.COWPLAY_CAR_UX_CASES&&!process.env.COWPLAY_CAR_UX_CASES.split(',').includes(name))return;const s=await startFixture(),b=await start();const frame=()=>b.evaluate('new Promise(r=>requestAnimationFrame(()=>requestAnimationFrame(r)))');try{await b.call('Page.addScriptToEvaluateOnNewDocument',{source});await b.viewport(1280,720);results.push({name,...await run(s,b,frame),browserErrors:b.errors});}catch(e){results.push({name,pass:false,error:String(e),browserErrors:b.errors});}finally{await b.close();await s.close();}}
async function confirm(b){await b.wait(()=>b.evaluate("!!document.querySelector('.action-confirmation[open]')"),'confirmation');await b.evaluate("document.querySelector('.confirmation-accept').click()");}
await scenario('another tab of the same browser can explicitly take over the picture',async(s,b,frame)=>{
 await s.fixture({mode:'takeover',auth:true,clientId:'other-screen'});await b.goto(s.origin+'/car?review=same-browser-takeover');await b.wait(()=>b.evaluate("document.querySelector('#c-title')?.textContent==='画面正在另一块屏幕显示'"),'another tab owns the picture');
 const before=(await s.fixture({})).wsAttempts;
 await b.evaluate("[...document.querySelectorAll('#c-body button')].find(x=>x.textContent==='在这块屏幕显示').click()");await b.wait(()=>b.evaluate("!!document.querySelector('.action-confirmation[open]')"),'explicit takeover warning');
 const prompt=await b.evaluate("document.querySelector('.action-confirmation').textContent");await b.evaluate("document.querySelector('.confirmation-cancel').click()");await frame();const cancelled=(await s.fixture({})).wsAttempts===before;
 await b.evaluate("[...document.querySelectorAll('#c-body button')].find(x=>x.textContent==='在这块屏幕显示').click()");await confirm(b);await b.wait(async()=>(await s.fixture({})).wsAttempts>before,'confirmed new socket');
 await s.fixture({mode:'session'});await b.wait(()=>b.evaluate("document.body.dataset.view==='session'&&reviewFrames>0"),'confirmed takeover receives and draws media');
 return {pass:cancelled&&prompt.includes('另一块屏幕将停止显示 CarPlay'),cancelled,prompt,drawn:true};
});
await scenario('late stop ACK must not stop a new authorization',async(s,b,frame)=>{
 await s.fixture({mode:'session',auth:true});await b.goto(s.origin+'/car?review=old-stop');await b.wait(()=>b.evaluate("document.body.dataset.view==='session'&&reviewFrames>0"),'initial video');
 await b.evaluate("window.reviewHold='disconnect';document.querySelector('#r-sheet').click()");await b.wait(()=>b.evaluate("!document.querySelector('#sheet').hidden"),'controls');await b.evaluate("document.querySelector('#sheet-stop').click()");await confirm(b);await b.wait(()=>b.evaluate("typeof window.reviewRelease==='function'"),'held ACK');
 await s.fixture({auth:false,stopped:false,mode:'session',clientId:'review-new-screen',wsText:{type:'error',code:'AUTH_REVOKED'}});await b.wait(()=>b.evaluate("!document.querySelector('#token-layer').hidden"),'revocation');
 await s.fixture({auth:true});await b.evaluate("for(const n of '123456')[...document.querySelectorAll('#keys button')].find(x=>x.textContent===n).click()");await b.wait(()=>b.evaluate("document.body.dataset.view==='session'&&document.querySelector('#token-layer').hidden"),'new authorization');
 const before=await b.evaluate("({view:document.body.dataset.view,scene:document.body.dataset.scene})");await b.evaluate('window.reviewRelease()');await frame();const after=await b.evaluate("({view:document.body.dataset.view,scene:document.body.dataset.scene,title:document.querySelector('#c-title').textContent})");const remote=await s.fixture({});return {pass:after.view==='session'&&remote.stopped===false,before,after,server:{mode:remote.mode,stopped:remote.stopped}};
});
await scenario('late connect ACK must not navigate away from a newer settings page',async(s,b,frame)=>{
 await s.fixture({mode:'connecting',auth:true,stopped:true,link:{bluetooth:'idle',wifi:'idle',carplay:'idle'}});await b.goto(s.origin+'/car?review=cross-page#settings/phones');await b.wait(()=>b.evaluate("[...document.querySelectorAll('#s-pane button')].some(x=>x.textContent==='连接')"),'saved phones');
 await b.evaluate("window.reviewHold='connect';[...document.querySelectorAll('#s-pane button')].find(x=>x.textContent==='连接').click()");await b.wait(()=>b.evaluate("typeof window.reviewRelease==='function'"),'held connect ACK');
 await b.evaluate("location.hash='settings/audio'");await b.wait(()=>b.evaluate("document.querySelector('#s-pane h1')?.textContent==='声音与通话'"),'newer settings page');await b.evaluate('window.reviewRelease()');await b.evaluate('new Promise(r=>setTimeout(r,250))');await frame();
 const after=await b.evaluate("({view:document.body.dataset.view,hash:location.hash,heading:document.querySelector('#s-pane h1')?.textContent})");return {pass:after.view==='settings'&&after.hash==='#settings/audio',after};
});
await scenario('a rejected reconnect must leave the existing picture decoding',async(s,b,frame)=>{
 await s.fixture({mode:'session',auth:true,cmdFail:true});await b.goto(s.origin+'/car?review=reconnect-fail');await b.wait(()=>b.evaluate("document.body.dataset.view==='session'&&reviewFrames>1"),'initial decoding');await b.evaluate("document.querySelector('#r-re').click()");await confirm(b);
 await b.wait(()=>b.evaluate("document.querySelector('.toast')?.textContent.includes('合成操作失败')"),'failed command feedback');const before=await b.evaluate('reviewFrames');await b.evaluate('new Promise(r=>setTimeout(r,700))');await frame();const after=await b.evaluate("({frames:reviewFrames,view:document.body.dataset.view,cover:document.querySelector('#cover-text').textContent,stall:document.querySelector('#stall-title').textContent})");return {pass:after.frames>before,beforeFrames:before,after};
});

await scenario('a connect state refresh must not navigate away from a newer page',async(s,b,frame)=>{
 await s.fixture({mode:'connecting',auth:true,stopped:true,link:{bluetooth:'idle',wifi:'idle',carplay:'idle'}});await b.goto(s.origin+'/car?review=poll-navigation#settings/phones');await b.wait(()=>b.evaluate("[...document.querySelectorAll('#s-pane button')].some(x=>x.textContent==='连接')"),'saved phones');
 await s.fixture({stateHold:true});await b.evaluate("[...document.querySelectorAll('#s-pane button')].find(x=>x.textContent==='连接').click()");await b.wait(async()=>(await s.fixture({})).pendingStates>0,'command state refresh held');
 await b.evaluate("location.hash='settings/audio'");await b.wait(()=>b.evaluate("document.querySelector('#s-pane h1')?.textContent==='声音与通话'"),'new page while state pending');await s.fixture({stateHold:false,releaseState:true});await b.evaluate('new Promise(r=>setTimeout(r,250))');await frame();
 const after=await b.evaluate("({view:document.body.dataset.view,hash:location.hash})");return {pass:after.view==='settings'&&after.hash==='#settings/audio',after};
});
await scenario('repeated clicks during one pending connect send one command',async(s,b,frame)=>{
 await s.fixture({mode:'connecting',auth:true,stopped:true,link:{bluetooth:'idle',wifi:'idle',carplay:'idle'}});await b.goto(s.origin+'/car?review=duplicate#settings/phones');await b.wait(()=>b.evaluate("[...document.querySelectorAll('#s-pane button')].some(x=>x.textContent==='连接')"),'saved phones');
 await b.evaluate("window.reviewHold='connect';[...document.querySelectorAll('#s-pane button')].find(x=>x.textContent==='连接').click()");await b.wait(()=>b.evaluate("typeof window.reviewRelease==='function'"),'pending connect');
 const before=(await s.fixture({})).cmds.filter(x=>x.cmd==='connect').length;await b.evaluate("[...document.querySelectorAll('#s-pane button')].find(x=>x.textContent==='连接').click()");await frame();const reads=(await s.fixture({})).stateReads;await b.wait(async()=>(await s.fixture({})).stateReads>reads,'one state poll while the explicit command is pending');await frame();const after=(await s.fixture({})).cmds.filter(x=>x.cmd==='connect').length;await b.evaluate('window.reviewRelease()');await frame();return {pass:before===1&&after===1,before,after};
});
await scenario('late connection-check results cannot label another iPhone as tested',async(s,b,frame)=>{
 await s.fixture({mode:'session',auth:true});await b.goto(s.origin+'/car?review=diagnostic#settings/diag');await b.wait(()=>b.evaluate("[...document.querySelectorAll('#s-pane button')].some(x=>x.textContent==='开始检查')"),'diagnostic action');
 await b.evaluate("window.reviewHoldPath='/api/selftest/segments';[...document.querySelectorAll('#s-pane button')].find(x=>x.textContent==='开始检查').click()");await b.wait(()=>b.evaluate("typeof window.reviewRelease==='function'"),'held selftest result');
 await s.fixture({currentPhone:'/org/bluez/hci0/dev_00_11_22_33_44_77'});const before=(await s.fixture({})).stateReads;await b.wait(async()=>(await s.fixture({})).stateReads>before,'changed phone observed');await frame();await b.evaluate('window.reviewRelease()');await b.evaluate('new Promise(r=>setTimeout(r,200))');await frame();
 await b.wait(()=>b.evaluate("[...document.querySelectorAll('#s-pane button')].some(x=>x.textContent==='开始检查')"),'new phone can run a new check');const text=await b.evaluate("document.querySelector('#s-pane').textContent");return {pass:!text.includes('合成验证通过')&&!text.includes('检查耗时 12 ms'),hasOldResult:text.includes('合成验证通过'),newCheckEnabled:await b.evaluate("[...document.querySelectorAll('#s-pane button')].some(x=>x.textContent==='开始检查'&&!x.disabled)")};
});

await scenario('late connection-check results cannot label a different screen as tested',async(s,b,frame)=>{
 await s.fixture({mode:'session',auth:true});await b.goto(s.origin+'/car?review=screen-diagnostic#settings/diag');await b.wait(()=>b.evaluate("[...document.querySelectorAll('#s-pane button')].some(x=>x.textContent==='开始检查')"),'diagnostic action');
 await b.evaluate("window.reviewHoldPath='/api/selftest/segments';[...document.querySelectorAll('#s-pane button')].find(x=>x.textContent==='开始检查').click()");await b.wait(()=>b.evaluate("typeof window.reviewRelease==='function'"),'held selftest result');
 await s.fixture({mode:'takeover'});const before=(await s.fixture({})).stateReads;await b.wait(async()=>(await s.fixture({})).stateReads>before,'changed screen observed');await frame();await b.evaluate('window.reviewRelease()');await b.evaluate('new Promise(r=>setTimeout(r,200))');await frame();
 await b.wait(()=>b.evaluate("[...document.querySelectorAll('#s-pane button')].some(x=>x.textContent==='开始检查')"),'new screen can run a new check');const text=await b.evaluate("document.querySelector('#s-pane').textContent");return {pass:!text.includes('合成验证通过')&&!text.includes('检查耗时 12 ms'),hasOldResult:text.includes('合成验证通过'),newCheckEnabled:await b.evaluate("[...document.querySelectorAll('#s-pane button')].some(x=>x.textContent==='开始检查'&&!x.disabled)")};
});


await scenario('late checks cannot label a different connecting target as tested',async(s,b,frame)=>{
 await s.fixture({mode:'connecting',auth:true,link:{bluetooth:'active',wifi:'idle',carplay:'idle'}});await b.goto(s.origin+'/car?review=connecting-target#settings/diag');await b.wait(()=>b.evaluate("[...document.querySelectorAll('#s-pane button')].some(x=>x.textContent==='开始检查')"),'diagnostic action');
 await b.evaluate("window.reviewHoldPath='/api/selftest/segments';[...document.querySelectorAll('#s-pane button')].find(x=>x.textContent==='开始检查').click()");await b.wait(()=>b.evaluate("typeof window.reviewRelease==='function'"),'held selftest');
 await s.fixture({currentPhone:'/org/bluez/hci0/dev_00_11_22_33_44_77'});const before=(await s.fixture({})).stateReads;await b.wait(async()=>(await s.fixture({})).stateReads>before,'new connecting target observed');await frame();await b.evaluate('window.reviewRelease()');await b.evaluate('new Promise(r=>setTimeout(r,250))');await frame();
 await b.wait(()=>b.evaluate("[...document.querySelectorAll('#s-pane button')].some(x=>x.textContent==='开始检查'&&!x.disabled)"),'new connecting target can start its own check');const text=await b.evaluate("document.querySelector('#s-pane').textContent");return {pass:!text.includes('合成验证通过'),hasOldResult:text.includes('合成验证通过'),newCheckEnabled:await b.evaluate("[...document.querySelectorAll('#s-pane button')].some(x=>x.textContent==='开始检查'&&!x.disabled)")};
});


await scenario('a reconnect ACK arriving after new media cannot freeze that recovered picture',async(s,b,frame)=>{
 await s.fixture({mode:'session',auth:true});await b.goto(s.origin+'/car?review=late-reconnect');await b.wait(()=>b.evaluate("document.body.dataset.view==='session'&&reviewFrames>1"),'initial decoding');await b.evaluate("window.reviewHold='reconnect';document.querySelector('#r-re').click()");await confirm(b);await b.wait(()=>b.evaluate("typeof window.reviewRelease==='function'"),'held reconnect ACK');
 await s.fixture({sessionReset:true});await b.wait(()=>b.evaluate("document.querySelector('#cover-text').textContent.includes('会话已结束')"),'old media session ended');const waiting=await b.evaluate("document.querySelector('#stall').textContent");const resetFrames=await b.evaluate('reviewFrames');await s.fixture({paused:false,resumeVideo:true});await b.wait(()=>b.evaluate(`reviewFrames>${resetFrames+2}`),'new media already recovered');
 const before=await b.evaluate('reviewFrames');await b.evaluate('window.reviewRelease()');await b.evaluate('new Promise(r=>setTimeout(r,700))');await frame();const after=await b.evaluate("({frames:reviewFrames,view:document.body.dataset.view,cover:document.querySelector('#cover-text').textContent})");return {pass:after.frames>before&&!waiting.includes('车机与 F50 的连接中断')&&waiting.includes('投屏通道已连接'),waiting,beforeFrames:before,after};
});


await scenario('a hung manual command reports uncertainty and releases retry after its bounded wait',async(s,b,frame)=>{
 await s.fixture({mode:'connecting',auth:true,stopped:true,link:{bluetooth:'idle',wifi:'idle',carplay:'idle'}});await b.goto(s.origin+'/car?review=timeout#settings/phones');await b.wait(()=>b.evaluate("[...document.querySelectorAll('#s-pane button')].some(x=>x.textContent==='连接')"),'saved phones');
 await b.evaluate("window.reviewHold='connect';[...document.querySelectorAll('#s-pane button')].find(x=>x.textContent==='连接').click()");await b.wait(()=>b.evaluate("typeof window.reviewRelease==='function'"),'hung command');
 // The command budget is deliberately 15 seconds (8s quiesce + 3s shutdown).
 // This new scenario observes that real deadline rather than changing product timing.
 await b.wait(()=>b.evaluate("document.querySelector('.toast')?.textContent.includes('操作结果未确认')"),'bounded command outcome',20000);
 const feedback=await b.evaluate("document.querySelector('.toast').textContent"),before=(await s.fixture({})).cmds.filter(x=>x.cmd==='connect').length;
 await b.evaluate("window.reviewHold=null;[...document.querySelectorAll('#s-pane button')].find(x=>x.textContent==='连接').click()");await b.wait(async()=>(await s.fixture({})).cmds.filter(x=>x.cmd==='connect').length>before,'same command can retry');await frame();
 return {pass:feedback.includes('请求超时')&&!feedback.includes('未执行'),feedback,retried:true};
});

await scenario('a failed repeated connection check cannot retain an old successful result',async(s,b,frame)=>{
 await s.fixture({mode:'session',auth:true});await b.goto(s.origin+'/car?review=check-retry#settings/diag');await b.wait(()=>b.evaluate("[...document.querySelectorAll('#s-pane button')].some(x=>x.textContent==='开始检查')"),'diagnostic action');
 await b.evaluate("[...document.querySelectorAll('#s-pane button')].find(x=>x.textContent==='开始检查').click()");await b.wait(()=>b.evaluate("document.querySelector('#s-pane').textContent.includes('合成验证通过')"),'first successful check');
 await b.evaluate("(()=>{const original=window.fetch;window.fetch=(input,init)=>String(input).includes('/api/selftest/segments')?Promise.resolve(new Response(JSON.stringify({error:'合成检查失败'}),{status:503,headers:{'Content-Type':'application/json'}})):original(input,init);[...document.querySelectorAll('#s-pane button')].find(x=>x.textContent==='再次检查').click()})()");
 await b.wait(()=>b.evaluate("document.querySelector('.toast')?.textContent.includes('合成检查失败')"),'current failure feedback');await frame();
 const text=await b.evaluate("document.querySelector('#s-pane').textContent");return {pass:!text.includes('合成验证通过'),oldResultRetained:text.includes('合成验证通过')};
});

await scenario('a rejected pairing cancellation keeps the active pairing scene and allows retry',async(s,b,frame)=>{
 await s.fixture({mode:'connecting',auth:true,phase:'Waiting',open:true,code:null,request:null,cmdFail:true});await b.goto(s.origin+'/car?review=cancel-failure');await b.wait(()=>b.evaluate("document.body.dataset.scene==='window'"),'active pairing window');
 await b.evaluate("[...document.querySelectorAll('#c-body button')].find(x=>x.textContent==='取消').click()");await b.wait(()=>b.evaluate("document.querySelector('.toast')?.textContent.includes('合成操作失败')"),'cancel failure feedback');await frame();
 const failed=await b.evaluate("({scene:document.body.dataset.scene,cancel:[...document.querySelectorAll('#c-body button')].some(x=>x.textContent==='取消'&&!x.disabled)})");
 await s.fixture({cmdFail:false});await b.evaluate("[...document.querySelectorAll('#c-body button')].find(x=>x.textContent==='取消').click()");await b.wait(async()=>!(await s.fixture({})).pairing.open,'retry actually closes pairing');
 return {pass:failed.scene==='window'&&failed.cancel,failed,retried:true};
});

const report={baseline:'7bce43d',synthetic:true,deviceOperations:false,results};const output=process.env.COWPLAY_CAR_UX_OUTPUT??path.join(os.tmpdir(),'cowplay-car-operation-ux-'+Date.now()+'.json');fs.writeFileSync(output,JSON.stringify(report,null,2)+'\n',{flag:'w'});console.log(JSON.stringify({...report,output},null,2));if(results.some(x=>!x.pass||x.browserErrors.length))process.exitCode=1;
