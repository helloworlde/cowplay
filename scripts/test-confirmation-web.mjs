// Real DOM confirmations; all endpoints, identities and media are synthetic.
import assert from 'node:assert/strict';
import {start} from './lib/ui-browser.mjs';
import {startFixture} from './lib/ui-fixture.mjs';
import {confirmation} from './lib/confirmation-contract.mjs';
const f=await startFixture(),b=await start(),checks=[];
const frame=()=>b.evaluate('new Promise(r=>requestAnimationFrame(()=>requestAnimationFrame(r)))');
const click=async selector=>{await b.wait(()=>b.evaluate(`!!document.querySelector(${JSON.stringify(selector)})&&!document.querySelector(${JSON.stringify(selector)}).disabled`),'enabled actionable control');await b.evaluate(`(()=>{const e=document.querySelector(${JSON.stringify(selector)});e.focus();e.click()})()`);await frame();};
const change=async(id,value)=>{await b.evaluate(`(()=>{const e=document.getElementById(${JSON.stringify(id)});e.value=${JSON.stringify(value)};e.dispatchEvent(new Event('change',{bubbles:true}))})()`);await frame();};
const snapshot=()=>f.fixture({});
const expectDialog=()=>b.wait(()=>b.evaluate("!!document.querySelector('.action-confirmation[open]')"),'confirmation before action');
try{
 await f.fixture({auth:true,mode:'session',phase:'closed',open:false,link:{bluetooth:'idle',wifi:'idle',carplay:'idle'},clear:true});
 await b.viewport(390,844);await b.goto(f.origin+'/car#settings/pic');
 await b.wait(()=>b.evaluate("!!document.querySelector('.picture-presets')"),'car settings');
 await click('.picture-presets button:nth-child(4)');await change('settings-picture-fps','30');
 const before=await snapshot();await click('#settings-picture-apply');await expectDialog();
 assert.equal((await snapshot()).prefsWrites,before.prefsWrites);assert.equal((await snapshot()).prefs.quality,'auto');
 assert.match(await b.evaluate("document.querySelector('#action-confirmation-description').textContent"),/断开.*重新连接/);
 assert.equal(await b.evaluate("(()=>{const r=document.querySelector('.action-confirmation').getBoundingClientRect();return r.left>=8&&r.right<=innerWidth-8&&r.top>=8&&r.bottom<=innerHeight-8})()"),true);
 await confirmation(b,'cancel');assert.equal((await snapshot()).prefsWrites,before.prefsWrites);
 assert.equal(await b.evaluate("document.querySelector('#settings-picture-fps').value"),'30');
 await click('#settings-picture-apply');await expectDialog();
 await b.evaluate("const e=document.querySelector('.confirmation-accept');e.click();e.click()");
 await b.wait(async()=>(await snapshot()).prefsWrites===before.prefsWrites+1,'one batch write');
 assert.equal((await snapshot()).prefs.fps,30);checks.push('car draft/cancel do not write; one explicit batch approval writes once');
 await b.wait(()=>b.evaluate("!document.querySelector('#settings-picture-reset').disabled"),'save finishes');
 await click('#settings-picture-reset');await expectDialog();const saved=(await snapshot()).prefsWrites;await confirmation(b,'cancel');assert.equal((await snapshot()).prefsWrites,saved);
 await click('.picture-advanced>summary');await change('settings-picture-max-fps','30');await expectDialog();
 assert.match(await b.evaluate("document.querySelector('.action-confirmation').textContent"),/所有屏幕/);await confirmation(b,'cancel');
 assert.equal((await (await fetch(f.origin+'/api/state')).json()).policy.max_fps,60);assert.equal(await b.evaluate("document.querySelector('#settings-picture-max-fps').value"),'60');
 await change('settings-picture-rhd','true');await click('#settings-picture-apply');await expectDialog();
 await f.fixture({prefsPatch:{ui_size:'large'}});await b.wait(()=>b.evaluate("!document.querySelector('.action-confirmation[open]')"),'external preference invalidates old approval');
 assert.equal((await snapshot()).prefsWrites,saved);checks.push('compatibility reset and shared limit cancel safely; stale preference approval is discarded');
 // A dialog must not survive a changed phone/session or revoked authorization.
 await click('.picture-presets button:nth-child(4)');await change('settings-picture-codec','hevc');await click('#settings-picture-apply');await expectDialog();
 await f.fixture({clientId:'new-confirmation-screen'});await b.wait(()=>b.evaluate("!document.querySelector('.action-confirmation[open]')"),'target change closes confirmation');
 assert.equal((await snapshot()).prefsWrites,saved);checks.push('target/authorization context changes do not send the old edit');
 await click('.picture-presets button:nth-child(4)');await change('settings-picture-codec','hevc');await click('#settings-picture-apply');await expectDialog();
 await f.fixture({mode:'connecting'});await b.wait(()=>b.evaluate("!document.querySelector('.action-confirmation[open]')"),'session change cancels confirmation');assert.equal((await snapshot()).prefsWrites,saved);
 await f.fixture({mode:'session'});await b.wait(()=>b.evaluate("document.querySelector('#s-tabs').textContent.includes('1 台在线')"),'phone returns');
 await click('#settings-picture-apply');await expectDialog();await b.evaluate("location.hash='settings/audio'");await b.wait(()=>b.evaluate("!document.querySelector('.action-confirmation[open]')"),'leaving settings cancels confirmation');assert.equal((await snapshot()).prefsWrites,saved);
 await b.evaluate("location.hash='settings/pic'");await b.wait(()=>b.evaluate("!!document.querySelector('#settings-picture-apply')"),'return to draft');await click('#settings-picture-apply');await expectDialog();
 await f.fixture({auth:false});await b.wait(()=>b.evaluate("document.querySelector('#token-layer').offsetParent!==null&&!document.querySelector('.action-confirmation[open]')"),'revocation cancels confirmation');assert.equal((await snapshot()).prefsWrites,saved);
 await f.fixture({auth:true});await b.evaluate("for(const digit of '123456')[...document.querySelectorAll('#keys button')].find(e=>e.textContent===digit).click()");await b.wait(()=>b.evaluate("document.querySelector('#token-layer').offsetParent===null"),'same screen reauthorized');
 checks.push('session changes, navigation and revoked authorization cancel old confirmations with zero writes');
 await b.goto(f.origin+'/phone#settings');await b.wait(()=>b.evaluate("document.documentElement.dataset.ready==='true'"),'phone settings');
 await click('#phone-pref-quality');
 await click('#phone-picture-controls .picture-presets button:nth-child(4)');await change('phone-picture-fps','60');
 await click('#phone-picture-apply');await expectDialog();await b.call('Input.dispatchKeyEvent',{type:'keyDown',key:'Escape',code:'Escape',windowsVirtualKeyCode:27});await b.call('Input.dispatchKeyEvent',{type:'keyUp',key:'Escape',code:'Escape',windowsVirtualKeyCode:27});
 await b.wait(()=>b.evaluate("!document.querySelector('.action-confirmation[open]')"),'Escape cancels just the confirmation');
 assert.equal(await b.evaluate("!!document.querySelector('#phone-picture-controls')"),true);assert.equal((await snapshot()).prefsWrites,saved);
 checks.push('phone Escape preserves its settings sheet and draft');
 await click('.bottom-sheet .sheet-head button');
 assert.equal(await b.evaluate('document.activeElement?.id'),'phone-pref-quality','closing a phone panel restores its preference trigger after rerendering');
 await click('#phone-pref-call');
 await click('.bottom-sheet [role=radio]:nth-child(2)');await expectDialog();await confirmation(b,'cancel');
 assert.equal(await b.evaluate("!!document.querySelector('.bottom-sheet [role=radio]')"),true,'cancel keeps the call settings open');assert.equal((await snapshot()).prefsWrites,saved);
 checks.push('call routing changes require approval; cancellation preserves the settings panel');

 await b.goto(f.origin+'/phone');await b.wait(()=>b.evaluate("document.documentElement.dataset.ready==='true'"),'phone status');
 await f.fixture({prefsPatch:{quality:'auto',resolution:'native',fps:60,codec:'h264'}});
 await b.wait(()=>b.evaluate("document.querySelector('#content [role=radio][aria-checked=true]')?.textContent==='自动'"),'current quick preset');
 await click('#content [role=radio][aria-checked=true]');assert.equal((await snapshot()).prefsWrites,saved);assert.equal(await b.evaluate("!!document.querySelector('.action-confirmation[open]')"),false);
 await b.evaluate("[...document.querySelectorAll('#content [role=radio]')].find(e=>e.textContent==='流畅').click()");await expectDialog();await confirmation(b,'cancel');assert.equal((await snapshot()).prefsWrites,saved);
 await b.evaluate("[...document.querySelectorAll('#content [role=radio]')].find(e=>e.textContent==='自定义').click()");await b.wait(()=>b.evaluate("!!document.querySelector('#phone-picture-controls')"),'custom quick preset opens the shared editor');assert.equal((await snapshot()).prefsWrites,saved);await click('.bottom-sheet .sheet-head button');
 checks.push('unchanged values do not prompt or write; quick presets confirm and custom opens its editor');
 for(const text of ['重连 CarPlay','刷新车机页面','断开 CarPlay']){
  const n=(await snapshot()).cmds.length;await b.evaluate(`[...document.querySelectorAll('#content button')].find(e=>e.textContent===${JSON.stringify(text)}).click()`);await expectDialog();assert.equal((await snapshot()).cmds.length,n);await confirmation(b,'cancel');assert.equal((await snapshot()).cmds.length,n);
 }
 await b.goto(f.origin+'/phone#iphones');await b.wait(()=>b.evaluate("document.documentElement.dataset.ready==='true'"),'phone list');
 for(const action of ['switch','add','forget']){
  const n=(await snapshot()).cmds.length;
  if(action==='switch')await b.evaluate("[...document.querySelectorAll('.phone-row button')].find(e=>e.textContent==='切换').click()");
  if(action==='add')await b.evaluate("[...document.querySelectorAll('#content button')].find(e=>e.textContent==='＋ 添加 iPhone').click()");
  if(action==='forget'){await b.evaluate("[...document.querySelectorAll('#content button')].find(e=>e.textContent==='编辑').click()");await frame();await click('.minus:not(:disabled)');}
  await expectDialog();assert.equal((await snapshot()).cmds.length,n);await confirmation(b,'cancel');assert.equal((await snapshot()).cmds.length,n);
 }
 checks.push('reconnect, refresh, disconnect, switch, pairing and forget all require one explicit decision');
 assert.deepEqual(b.errors,[]);console.log(JSON.stringify({checks,errors:b.errors},null,2));
}catch(error){console.log(JSON.stringify({error:String(error),shot:await b.shot('confirmation-failure'),errors:b.errors}));throw error;}
finally{await b.close();await f.close();}
