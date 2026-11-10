// Synthetic API, real DOM; no F50 or real phone is accessed.
import assert from 'node:assert/strict';
import {start} from './lib/ui-browser.mjs';
import {startFixture} from './lib/ui-fixture.mjs';
const service=await startFixture(),b=await start(),checks=[],shots=[];
const value=s=>b.evaluate(s),fixture=service.fixture;
const NEW='/org/bluez/hci0/dev_00_11_22_33_44_99',OFFLINE='/org/bluez/hci0/dev_00_11_22_33_44_88';
const commands=async()=> (await fixture({})).cmds;
async function load(route,w=390,h=844){
  await b.viewport(w,h);await b.goto(service.origin+route);
  await b.wait(()=>value("document.documentElement.dataset.ready==='true'||!!document.querySelector('#c-title')?.textContent"),'page');
}
async function click(text){
  assert.ok(await value(`(()=>{const e=[...document.querySelectorAll('button,summary')].find(e=>e.offsetParent!==null&&!e.disabled&&e.textContent.trim()===${JSON.stringify(text)});if(!e)return false;e.click();return true})()`),text);
}
try{
  for(const [route,mode,label] of [['/pair','connecting','pairing page'],['/car','onboard','car onboarding'],['/car?window','connecting','car add window'],['/phone#iphones','connecting','phone add sheet']]){
    await fixture({mode,auth:true,phase:'Waiting',open:true,device:null,newPhone:null,pairingError:null,adapterName:null,adapterError:'合成名称读取失败',clear:true});
    await load(route,route.startsWith('/car')?1280:390,route.startsWith('/car')?720:844);
    if(route.startsWith('/phone'))await click('＋ 添加 iPhone');
    await b.wait(()=>value("document.body.textContent.includes('蓝牙名称暂时未知')"),label+' unknown name');
    assert.equal(await value("document.body.textContent.includes('“cowplay”')"),false,label+' must not invent the configured name');
    await fixture({adapterName:'SPREADTRUM BT',adapterError:null});
    await b.wait(()=>value("document.body.textContent.includes('“SPREADTRUM BT”')"),label+' actual name recovery');
    checks.push(label+' shows unknown on name read failure and updates instructions to the actual readback');
  }
  await fixture({adapterName:'legacy-name',adapter:{alias:'actual-alias',name:'controller-name',powered:true,discoverable:true,pairable:true}});
  await b.wait(()=>value("document.body.textContent.includes('“actual-alias”')"),'BlueZ Alias takes precedence');
  assert.equal(await value("document.body.textContent.includes('“legacy-name”')"),false);
  checks.push('pairing instructions prefer the actual BlueZ Alias over a legacy field');
  await fixture({adapterName:'cowplay',adapter:{alias:'cowplay',name:'cowplay',controller_name:'SPREADTRUM BT',eir_name:null,powered:true,discoverable:true,pairable:true}});
  await b.wait(()=>value("document.body.textContent.includes('“SPREADTRUM BT”')"),'hardware name wins over matching software Alias');
  assert.equal(await value("document.body.textContent.includes('“cowplay”')"),false);
  await fixture({adapter:{alias:'cowplay',name:'cowplay',controller_name:null,eir_name:null,powered:true,discoverable:true,pairable:true}});
  await b.wait(()=>value("document.body.textContent.includes('蓝牙名称暂时未知')"),'missing hardware readback stays unknown');
  assert.equal(await value("document.body.textContent.includes('“cowplay”')"),false);
  checks.push('HCI name takes precedence and failed hardware readback cannot fall back to the brand or Alias');
  await fixture({adapterName:'cowplay',adapter:null,adapterError:null,open:false});
  await fixture({mode:'connecting',auth:true,phase:'closed',clear:true});await load('/pair');
  await click('日志与调试信息');
  await b.wait(()=>value("document.querySelector('.connection-debug-log').textContent.includes('合成测试日志')"),'logs');
  assert.equal(await value("document.querySelector('.connection-debug').textContent.includes('synthetic-only')"),false);
  assert.ok(await value("document.querySelector('.connection-debug-log').textContent.includes('\\n')"),'log entries use actual line breaks');
  await fixture({stateFail:true});
  await b.wait(()=>value("document.body.textContent.includes('合成状态读取失败')"),'state failure');
  assert.ok(await value("document.querySelector('.connection-debug').open"));
  await fixture({stateFail:false});
  await b.wait(()=>value("!document.body.textContent.includes('合成状态读取失败')"),'state recovery');
  checks.push('expanded logs persist through state failure and contain no QR/password payload');
  await fixture({logsFail:true});await click('刷新日志');
  await b.wait(()=>value("document.querySelector('.connection-debug-note').textContent.includes('合成日志读取失败')"),'logs failure');
  await fixture({logsFail:false});await click('刷新日志');
  await b.wait(()=>value("document.querySelector('.connection-debug-note').textContent.includes('最近 80 条')"),'logs retry');
  checks.push('log read failure is visible and manually retryable');
  await fixture({logsHold:true});await click('刷新日志');
  await b.wait(async()=> (await fixture({})).heldLogs,'held old log response');
  await fixture({auth:false});
  await b.wait(()=>value("!!document.querySelector('.auth')"),'authorization loss');
  await fixture({clientId:'new-diagnostics-phone'});
  await value("document.querySelector('.auth input').value='123456';document.querySelector('.auth').requestSubmit()");
  await b.wait(()=>value("!!document.querySelector('.connection-debug')"),'readmitted');
  await fixture({releaseLogs:'OLD-PRIVATE-LOG'});
  await b.wait(()=>value("document.querySelector('.connection-debug-log').textContent.includes('合成测试日志')"),'new identity logs');
  assert.equal(await value("document.body.textContent.includes('OLD-PRIVATE-LOG')"),false);
  await value("document.querySelector('.connection-debug').scrollIntoView({block:'start'})");
  shots.push(await b.shot('pairing-diagnostics-390'));
  checks.push('authorization loss clears cached diagnostics and rejects late old log response');
  await fixture({mode:'connecting',phase:'Cancelled',open:false,device:NEW,newPhone:null,pairingError:'新手机配对被拒绝',link:{bluetooth:'fail',wifi:'idle',carplay:'idle',error:{step:'bluetooth',title:'新手机配对被拒绝',hint:'重新开始配对',ts_ms:Date.now()}},clear:true});
  await load('/car',1280,720);
  await b.wait(()=>value("document.querySelector('#c-lead').textContent.includes('新手机配对被拒绝')"),'pairing error');
  await value("{const original=Date.now;Date.now=()=>original()+25000}");
  await b.wait(()=>value("!!document.querySelector('.connection-debug')"),'car diagnostics');
  assert.ok(await value("!document.querySelector('#c-title').textContent.includes('测试 iPhone')"));
  assert.equal((await commands()).filter(c=>c.cmd==='connect').length,0);
  shots.push(await b.shot('car-new-phone-failure-1280'));
  checks.push('failed unsaved new phone never falls back to a saved old phone');
  await fixture({stateFail:true});
  await b.wait(()=>value("document.body.textContent.includes('状态更新失败')"),'car stale state warning');
  await fixture({stateFail:false});
  await b.wait(()=>value("!document.body.textContent.includes('状态更新失败')"),'car state recovery');
  checks.push('car state API failure explicitly marks previous state as stale');
  await fixture({mode:'connecting',phase:'Paired',pairingError:null,newPhone:NEW,device:NEW,link:{bluetooth:'fail',wifi:'idle',carplay:'idle',error:{step:'bluetooth',title:'新手机自动连接失败',hint:'请重试',ts_ms:Date.now()}},clear:true});
  await load('/car?failed',800,480);
  await b.wait(()=>value("document.querySelector('#c-title').textContent==='新 iPhone'"),'new target');
  await click('重新连接');
  await b.wait(async()=> (await commands()).some(c=>c.cmd==='connect'&&c.id===NEW),'explicit retry');
  await click('日志与调试信息');
  await b.wait(()=>value("document.querySelector('.connection-debug-log').textContent.includes('合成测试日志')"),'car logs');
  await value("document.querySelector('.connection-debug-log').scrollIntoView({block:'end'})");
  assert.ok(await value('document.documentElement.scrollWidth<=innerWidth'));
  shots.push(await b.shot('car-diagnostics-800'));
  checks.push('retry keeps the new phone target and expanded car diagnostics fit compact width');
  for(const route of ['/phone#iphones','/car#settings/phones']){
    await fixture({mode:'handoff',currentPhone:'/org/bluez/hci0/dev_00_11_22_33_44_66',phase:'closed',device:null,newPhone:null,pairingError:null,link:{bluetooth:'ok',wifi:'active',carplay:'active',error:null},clear:true});
    await load(route,route.startsWith('/car')?1280:390,route.startsWith('/car')?720:844);
    await b.wait(()=>value("[...document.querySelectorAll('.row')].some(e=>e.textContent.includes('离线 iPhone'))"),'saved list');
    assert.ok(await value("(()=>{const row=[...document.querySelectorAll('.row')].find(e=>e.textContent.includes('离线 iPhone'));const button=[...row.querySelectorAll('button')].find(e=>e.textContent.includes('切'));if(!button||button.disabled)return false;button.click();return true})()"),route);
    await b.wait(async()=> (await commands()).some(c=>c.cmd==='switchPhone'&&c.id===OFFLINE),'switch offline saved phone');
    assert.equal((await commands()).filter(c=>c.cmd==='disconnect').length,0);
    checks.push(route+' directly switches away from a connecting phone without manual disconnect');
  }
  assert.deepEqual(b.errors,[]);console.log(JSON.stringify({checks,shots,errors:b.errors},null,2));
}catch(e){console.log(JSON.stringify({error:String(e),text:await value('document.body.innerText'),shot:await b.shot('connection-diagnostics-failure')}));throw e}
finally{await b.close();await service.close()}
