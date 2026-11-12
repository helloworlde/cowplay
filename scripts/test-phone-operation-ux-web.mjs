// Delayed replies in the phone console. All responses, tokens and identities are synthetic.
import assert from 'node:assert/strict';
import {start} from './lib/ui-browser.mjs';
import {startFixture} from './lib/ui-fixture.mjs';

const results=[];
async function scenario(name,run){
 const f=await startFixture();let b;
 try{
  b=await start();await b.viewport(390,844);
  await b.call('Page.addScriptToEvaluateOnNewDocument',{source:`(()=>{
   const fetch=window.fetch.bind(window);window.qaHold={};window.qaFail={};window.qaHeld=[];window.qaCalls=[];window.qaVisible=true;
   Object.defineProperty(document,'hidden',{get:()=>!window.qaVisible});Object.defineProperty(document,'visibilityState',{get:()=>window.qaVisible?'visible':'hidden'});
   window.fetch=async(...args)=>{
    const path=new URL(args[0],location.href).pathname,hold=window.qaHold[path],failure=window.qaFail[path];
    window.qaCalls.push({path,body:args[1]?.body});
    if(failure)return new Response(JSON.stringify({error:failure}),{status:409,headers:{'Content-Type':'application/json'}});
    const response=await fetch(...args);
    return hold?new Promise(resolve=>window.qaHeld.push({path,response,resolve})):response;
   };
   window.qaRelease=(path,body=null,status=200,index=0)=>{
    const entry=window.qaHeld.filter(e=>e.path===path)[index];if(!entry)throw Error('No held reply for '+path);
    window.qaHeld.splice(window.qaHeld.indexOf(entry),1);
    entry.resolve(body===null?entry.response:new Response(JSON.stringify(body),{status,headers:{'Content-Type':'application/json'}}));
   };
  })()`});
  await f.fixture({auth:true,mode:'session',phase:'closed',open:false,clear:true});
  const value=expression=>b.evaluate(expression);
  const frame=()=>value('new Promise(r=>requestAnimationFrame(()=>requestAnimationFrame(r)))');
  const click=async label=>{assert.ok(await value(`(()=>{const e=[...document.querySelectorAll('button,a')].find(e=>e.offsetParent!==null&&!e.disabled&&e.textContent.trim()===${JSON.stringify(label)});if(!e)return false;e.click();return true})()`),label);await frame();};
  const route=async page=>{await value(`location.hash=${JSON.stringify(page)}`);await b.wait(()=>value(`document.querySelector('#tabs [aria-current=page]')?.getAttribute('aria-label')===${JSON.stringify({status:'状态',settings:'设置',iphones:'iPhone',diag:'诊断',log:'诊断'}[page])}&&location.hash===${JSON.stringify('#'+page)}`),'route '+page);await frame();};
  const load=async page=>{await b.goto(f.origin+'/phone#'+page);await b.wait(()=>value("document.documentElement.dataset.ready==='true'&&!document.querySelector('.auth')"),'phone console');};
  const hold=path=>value(`window.qaHold[${JSON.stringify(path)}]=true`);
  const held=path=>b.wait(()=>value(`window.qaHeld.some(e=>e.path===${JSON.stringify(path)})`),'held '+path);
  const release=(path,body=null,status=200,index=0)=>value(`window.qaRelease(${JSON.stringify(path)},${JSON.stringify(body)},${status},${index})`);
  const tokenVisible=()=>value("document.querySelector('#content')?.textContent.includes('123456')??false");
  await run({b,f,value,frame,click,route,load,hold,held,release,tokenVisible});
  assert.deepEqual(b.errors,[]);results.push({name,status:'passed'});
 }catch(error){results.push({name,status:'failed',error:String(error),shot:b?await b.shot('phone-operation-'+name):null,errors:b?.errors});}
 finally{if(b)await b.close();await f.close();}
}

await scenario('token-navigation-and-new-request',async({value,frame,click,route,load,hold,held,release,tokenVisible})=>{
 await load('settings');await hold('/api/token');await click('显示令牌');await held('/api/token');
 await route('status');await route('settings');await release('/api/token');await frame();
 assert.equal(await tokenVisible(),false,'a reply from the previous settings visit cannot reveal the token');
 await click('显示令牌');await held('/api/token');assert.equal(await value("[...document.querySelectorAll('button')].find(e=>e.textContent==='正在读取令牌…')?.disabled"),true);await release('/api/token');await frame();
 assert.equal(await tokenVisible(),true,'a fresh explicit request still works');await click('隐藏令牌');assert.equal(await tokenVisible(),false);
});

await scenario('token-hidden-page',async({value,frame,click,load,hold,held,release,tokenVisible})=>{
 await load('settings');await hold('/api/token');await click('显示令牌');await held('/api/token');
 await value("window.qaVisible=false;document.dispatchEvent(new Event('visibilitychange'))");await release('/api/token');await frame();
 assert.equal(await tokenVisible(),false,'a hidden page cannot reveal a delayed token');
 await value("window.qaVisible=true;document.dispatchEvent(new Event('visibilitychange'))");await click('显示令牌');await held('/api/token');await release('/api/token');await frame();
 assert.equal(await tokenVisible(),true);
 await value("window.qaVisible=false;document.dispatchEvent(new Event('visibilitychange'))");
 assert.equal(await tokenVisible(),false,'backgrounding also clears an already rendered token immediately');
});

await scenario('token-revoked-and-readmitted',async({b,f,value,frame,click,load,hold,held,release,tokenVisible})=>{
 await load('settings');await hold('/api/token');await click('显示令牌');await held('/api/token');
 await f.fixture({auth:false});await b.wait(()=>value("!!document.querySelector('.auth')"),'authorization revoked');
 await value("document.querySelector('.auth input').value='123456';document.querySelector('.auth').requestSubmit()");
 await b.wait(()=>value("!document.querySelector('.auth')&&!![...document.querySelectorAll('button')].find(e=>e.textContent==='显示令牌')"),'same client readmitted');
 await release('/api/token');await frame();assert.equal(await tokenVisible(),false,'old authorization cannot reveal a token after readmission');
});

await scenario('policy-double-click-and-failure',async({b,f,value,frame,load,hold,held,release})=>{
 await load('iphones');await hold('/api/policy');
 await value("const e=document.querySelector('[role=switch]');e.click();e.click()");await held('/api/policy');
 assert.equal(await value("window.qaCalls.filter(e=>e.path==='/api/policy').length"),1,'a pending write cannot be submitted twice');
 assert.equal(await value("document.querySelector('[role=switch]').disabled"),true);
 await release('/api/policy');await b.wait(()=>value("!document.querySelector('[role=switch]').disabled"),'policy saved');
 assert.equal(await value("document.querySelector('[role=switch]').getAttribute('aria-checked')"),'false');
 assert.equal((await(await fetch(f.origin+'/api/state')).json()).policy.ask_when_multi,false,'UI uses the acknowledged server value');
 await value("window.qaFail['/api/policy']='合成自动连接保存失败';document.querySelector('[role=switch]').click()");
 await b.wait(()=>value("document.querySelector('#content').textContent.includes('合成自动连接保存失败')&&!document.querySelector('[role=switch]').disabled"),'failure is visible and retryable');
 assert.equal(await value("document.querySelector('[role=switch]').getAttribute('aria-checked')"),'false');
 await value("delete window.qaFail['/api/policy'];document.querySelector('[role=switch]').click()");await held('/api/policy');await release('/api/policy');await frame();
 await b.wait(()=>value("document.querySelector('[role=switch]').getAttribute('aria-checked')==='true'&&!document.querySelector('[role=switch]').disabled"),'retry saved');
});

await scenario('policy-old-failure-does-not-replace-new-page',async({value,frame,load,route,hold,held,release})=>{
 await load('iphones');await hold('/api/policy');await value("document.querySelector('[role=switch]').click()");await held('/api/policy');
 await route('settings');await release('/api/policy',{error:'过期自动连接失败'},409);await frame();
 assert.equal(await value("document.querySelector('#content').textContent.includes('过期自动连接失败')"),false);
});

await scenario('logs-old-failure-does-not-replace-new-page',async({value,frame,click,load,route,hold,held,release})=>{
 await load('log');await hold('/api/logs');await click('原始');await held('/api/logs');await route('settings');
 await release('/api/logs',{error:'过期日志失败'},409);await frame();
 assert.equal(await value("document.querySelector('#content').textContent.includes('过期日志失败')"),false);
});

await scenario('logs-newest-request-wins',async({b,value,frame,click,load,hold,release})=>{
 await load('log');await hold('/api/logs');await click('原始');
 await b.wait(()=>value("window.qaHeld.filter(e=>e.path==='/api/logs').length===1"),'all log request');await click('警告');
 await b.wait(()=>value("window.qaHeld.filter(e=>e.path==='/api/logs').length===2"),'warning request');await click('全部');
 await b.wait(()=>value("window.qaHeld.filter(e=>e.path==='/api/logs').length===3"),'latest all request');
 const entries=msg=>({entries:[{ts_ms:Date.now(),level:'INFO',msg,target:'test',text:msg}]});
 await release('/api/logs',entries('最新日志'),200,2);await frame();assert.equal(await value("document.querySelector('.raw-log').textContent.includes('最新日志')"),true);
 await release('/api/logs',entries('旧日志不应回来'));await frame();assert.equal(await value("document.querySelector('.raw-log').textContent.includes('最新日志')"),true,'returning to the same filter does not revive an earlier response');
 await release('/api/logs',{error:'过时筛选失败'},409);await frame();assert.equal(await value("document.querySelector('#content').textContent.includes('过时筛选失败')"),false);
});

await scenario('logs-retry-clears-old-error',async({b,value,click,load})=>{
 await load('log');await value("window.qaFail['/api/logs']='合成日志读取失败'");await click('原始');
 await b.wait(()=>value("document.querySelector('#content').textContent.includes('合成日志读取失败')"),'log failure');
 await value("delete window.qaFail['/api/logs']");await click('全部');
 await b.wait(()=>value("document.querySelector('.raw-log').textContent.includes('合成测试日志')&&!document.querySelector('#content').textContent.includes('日志读取失败：')"),'log retry recovers');
});

for(const kind of ['diagnostics','logs'])await scenario('export-late-failure-'+kind,async({b,value,frame,click,route,load,hold,held,release})=>{
 const path=kind==='diagnostics'?'/api/diag/bundle':'/api/logs/download';
 await load(kind==='diagnostics'?'diag':'log');await hold(path);await click(kind==='diagnostics'?'导出诊断包…':'分享');await held(path);
 await route('settings');await release(path,{error:'过期导出失败'},409);await frame();
 assert.equal(await value("document.querySelector('#content').textContent.includes('过期导出失败')"),false);
 await route(kind==='diagnostics'?'diag':'log');await value(`window.qaFail[${JSON.stringify(path)}]='合成导出失败'`);await click(kind==='diagnostics'?'导出诊断包…':'分享');
 await b.wait(()=>value("document.querySelector('#content').textContent.includes('合成导出失败')"),'current export failure');
 await value(`delete window.qaFail[${JSON.stringify(path)}];delete window.qaHold[${JSON.stringify(path)}]`);await click(kind==='diagnostics'?'导出诊断包…':'分享');
 await b.wait(()=>value("document.querySelector('.toast')?.textContent.includes('已导出')&&!document.querySelector('#content').textContent.includes('合成导出失败')"),'export retry clears the old error');
});

await scenario('connection-check-result-follows-phone-and-authorization',async({b,f,value,click,load})=>{
 await load('diag');await click('运行连接检查');await b.wait(()=>value("document.querySelector('#content').textContent.includes('检查耗时 12 ms')"),'check completed');
 await f.fixture({newPhone:'/org/bluez/hci0/dev_00_11_22_33_44_99'});
 await b.wait(()=>value("!document.querySelector('#content').textContent.includes('检查耗时 12 ms')"),'new phone clears the old measurement');
 await click('运行连接检查');await b.wait(()=>value("document.querySelector('#content').textContent.includes('检查耗时 12 ms')"),'new phone checked');
 await f.fixture({auth:false});await b.wait(()=>value("!!document.querySelector('.auth')"),'check owner revoked');
 await value("document.querySelector('.auth input').value='123456';document.querySelector('.auth').requestSubmit()");
 await b.wait(()=>value("!document.querySelector('.auth')&&document.querySelector('#content h1')?.textContent==='诊断'"),'readmitted diagnosis');
 assert.equal(await value("document.querySelector('#content').textContent.includes('检查耗时 12 ms')"),false);
});

await scenario('connection-check-pairing-generation-changes',async({b,f,value,click,load})=>{
 await load('diag');await click('运行连接检查');await b.wait(()=>value("document.querySelector('#content').textContent.includes('检查耗时 12 ms')"),'initial generation checked');
 await f.fixture({generation:44});await b.wait(()=>value("!document.querySelector('#content').textContent.includes('检查耗时 12 ms')"),'a new pairing generation clears the old result');
});

await scenario('connection-check-failure-remains-retryable',async({b,value,click,load})=>{
 await load('diag');await click('运行连接检查');await b.wait(()=>value("document.querySelector('#content').textContent.includes('检查耗时 12 ms')"),'initial check');
 await value("window.qaFail['/api/selftest/segments']='合成连接检查失败'");await click('运行连接检查');
 await b.wait(()=>value("document.querySelector('#content').textContent.includes('合成连接检查失败')"),'failure shown');
 assert.equal(await value("document.querySelector('#content').textContent.includes('检查耗时 12 ms')"),false,'a failed new check cannot keep showing the previous successful result');
 await value("delete window.qaFail['/api/selftest/segments']");await click('运行连接检查');
 await b.wait(()=>value("document.querySelector('#content').textContent.includes('检查耗时 12 ms')&&!document.querySelector('#content').textContent.includes('合成连接检查失败')"),'retry succeeds');
});

for(const failed of [false,true])await scenario('connection-check-late-'+(failed?'failure':'result'),async({b,f,value,frame,click,load,hold,held,release})=>{
 await load('diag');await hold('/api/selftest/segments');await click('运行连接检查');await held('/api/selftest/segments');
 await f.fixture({mode:'connecting',newPhone:null,link:{bluetooth:'idle',wifi:'idle',carplay:'idle'}});
 await b.wait(()=>value("!![...document.querySelectorAll('#content button')].find(e=>e.textContent==='运行连接检查'&&!e.disabled)"),'new session clears pending check');
 await release('/api/selftest/segments',failed?{error:'旧会话检查失败'}:null,failed?409:200);await frame();
 assert.equal(await value("/检查耗时 12 ms|旧会话检查失败/.test(document.querySelector('#content').textContent)"),false);
});

await scenario('preference-timeout-after-server-save-reports-an-uncertain-result',async({b,f,value,load})=>{
 await load('settings');await value("[...document.querySelectorAll('#content button')].find(e=>e.textContent.startsWith('画质')).click()");await b.wait(()=>value("!!document.querySelector('#phone-picture-controls')"),'quality editor');
 await value("(()=>{const original=window.fetch;window.fetch=async(input,init)=>{const result=await original(input,init);if(String(input).includes('/api/prefs'))throw new DOMException('合成响应超时','TimeoutError');return result;};document.querySelector('#phone-picture-controls .picture-presets button:nth-child(2)').click();document.querySelector('#phone-picture-apply').click()})()");
 await b.wait(()=>value("!!document.querySelector('.action-confirmation[open]')"),'save confirmation');await value("document.querySelector('.confirmation-accept').click()");
 await b.wait(()=>value("/未保存|保存结果未确认/.test(document.querySelector('.bottom-sheet').textContent)"),'save feedback');
 assert.equal((await f.fixture({})).prefs.quality,'smooth','the backend did save before the response became uncertain');
 assert.match(await value("document.querySelector('.bottom-sheet').textContent"),/保存结果未确认/);
});
await scenario('preference-network-loss-after-server-save-reports-an-uncertain-result',async({b,f,value,load})=>{
 await load('settings');await value("[...document.querySelectorAll('#content button')].find(e=>e.textContent.startsWith('画质')).click()");await b.wait(()=>value("!!document.querySelector('#phone-picture-controls')"),'quality editor');
 await value("(()=>{const original=window.fetch;window.fetch=async(input,init)=>{const result=await original(input,init);if(String(input).includes('/api/prefs'))throw new TypeError('Failed to fetch');return result;};document.querySelector('#phone-picture-controls .picture-presets button:nth-child(2)').click();document.querySelector('#phone-picture-apply').click()})()");
 await b.wait(()=>value("!!document.querySelector('.action-confirmation[open]')"),'save confirmation');await value("document.querySelector('.confirmation-accept').click()");
 await b.wait(()=>value("/未保存|保存结果未确认/.test(document.querySelector('.bottom-sheet').textContent)"),'save feedback');
 assert.equal((await f.fixture({})).prefs.quality,'smooth','the backend did save before the response became uncertain');
 assert.match(await value("document.querySelector('.bottom-sheet').textContent"),/保存结果未确认/);
});

console.log(JSON.stringify({results},null,2));if(results.some(r=>r.status==='failed'))process.exitCode=1;
