// Synthetic receiver state with real LuCI and optionally the installed theme CSS.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import {start} from './lib/ui-browser.mjs';
import {startFixture} from './lib/ui-fixture.mjs';
import {startLuciFixture} from './lib/luci-fixture.mjs';

assert.ok(process.env.LUCI_RUNTIME_DIR, 'official LuCI runtime required');
const menu=JSON.parse(fs.readFileSync(new URL('../luci-app-cowplay/root/usr/share/luci/menu.d/luci-app-cowplay.json',import.meta.url)));
assert.equal(menu['admin/services/cowplay/tesla'].title,undefined,'direct Tesla route must not add a second native LuCI tab row');
assert.equal(menu['admin/services/cowplay/tesla'].action.path,'cowplay/tesla');
const service=await startFixture(),luci=await startLuciFixture(process.env.LUCI_RUNTIME_DIR,service.origin,{darkMode:process.env.LUCI_TEST_DARK==='1'}),b=await start(),checks=[],shots=[];
const route='/cgi-bin/luci/admin/services/cowplay';
const labels=()=>b.evaluate("[...document.querySelectorAll('.cw-tabs a')].map(a=>a.textContent)");
const settle=()=>b.evaluate("Promise.all(document.getAnimations().filter(a=>a.effect.getTiming().iterations!==Infinity).map(a=>a.finished.catch(()=>{})))");
const metrics=()=>b.evaluate("(()=>{const r=document.querySelector('.cw').getBoundingClientRect(),s=getComputedStyle(document.querySelector('.cw .cbi-section'));return {x:r.x,width:r.width,title:document.querySelector('.cw-head h2').textContent,card:[s.backgroundColor,s.borderRadius,s.padding,s.fontFamily]}})()");
try {
 await service.fixture({networkPatch:{platform:'f50',luci:luci.origin+route}});
 for(const [w,h] of [[1440,1000],[390,844]]) {
  await b.viewport(w,h);await b.goto(luci.origin+route+'#overview');
  await b.wait(()=>b.evaluate("!!document.querySelector('.cw .cbi-section')"),'overview');
  await b.wait(()=>b.evaluate("getComputedStyle(document.querySelector('.cw')).maxWidth==='1200px'"),'shared styles loaded');
  await settle();const before=await metrics(),nav=await labels();assert.equal(nav.length,11);
  shots.push(await b.shot('luci-overview-'+w));
  await b.evaluate("[...document.querySelectorAll('.cw-tabs a')].find(a=>a.textContent==='Tesla 车辆').click()");
  await b.wait(()=>b.evaluate("location.pathname.endsWith('/cowplay/tesla')&&!!document.querySelector('.cw-tesla-step')"),'Tesla page actually loaded');
  await b.wait(()=>b.evaluate("getComputedStyle(document.querySelector('.cw')).maxWidth==='1200px'"),'Tesla shared styles loaded');
  await settle();const after=await metrics();assert.deepEqual(after,before,'both pages share header, width and native section styling');
  assert.deepEqual(await labels(),nav,'both pages expose the same flat navigation');
  assert.equal(await b.evaluate("document.querySelectorAll('.cw-tabs .cbi-tab').length"),1);
  assert.equal(await b.evaluate("document.querySelector('.cw-tabs .cbi-tab a').textContent"),'Tesla 车辆');
  assert.equal(await b.evaluate("[...document.querySelectorAll('.cw link[rel=stylesheet]')].every(l=>new URL(l.href).searchParams.has('v'))"),true,'updated styles have a versioned URL');
  assert.ok(await b.evaluate('document.documentElement.scrollWidth<=innerWidth'),'no horizontal page overflow');
  shots.push(await b.shot('luci-tesla-'+w));
  await b.evaluate("document.querySelector('.cw-tabs a[data-target=overview]').click()");
  await b.wait(()=>b.evaluate("location.pathname.endsWith('/cowplay')&&location.hash==='#overview'&&!!document.querySelector('.cw-endpoints')"),'return to overview');
  checks.push(w+'px: same header, all 11 tabs, width and cards; real forward/back navigation');
 }
 for(const [w,h] of [[1280,720],[800,480]]) {
  await b.viewport(w,h);await b.goto(service.origin+'/car?held-tesla='+w+'#settings/pic');
  await b.wait(()=>b.evaluate("!!document.querySelector('#settings-tab-tesla')"),'car Tesla entry');
  const point=await b.evaluate("(()=>{const a=window.__heldTesla=document.querySelector('#settings-tab-tesla');a.scrollIntoView({block:'nearest'});const r=a.getBoundingClientRect();return {x:r.x+r.width/2,y:r.y+r.height/2}})()");
  const reads=(await service.fixture({})).stateReads;
  await b.call('Input.dispatchMouseEvent',{type:'mousePressed',...point,button:'left',clickCount:1});
  await b.wait(async()=>(await service.fixture({})).stateReads>reads+1,'two state refreshes while link is held');
  await b.evaluate('new Promise(r=>requestAnimationFrame(()=>requestAnimationFrame(r)))');
  assert.equal(await b.evaluate("window.__heldTesla.isConnected&&window.__heldTesla===document.querySelector('#settings-tab-tesla')"),true,'polling preserves the pressed link');
  await b.call('Input.dispatchMouseEvent',{type:'mouseReleased',...point,button:'left',clickCount:1});
  await b.wait(()=>b.evaluate("location.pathname==='/car'&&location.hash==='#settings/tesla'&&!!document.querySelector('#tesla-unlock')"),'native release opens Tesla in the same window');
  checks.push(w+'px: press -> two polls -> release really opens Tesla management');
 }
 await b.goto(luci.origin+'/cgi-bin/luci/admin/services/cowplay/tesla');await b.wait(()=>b.evaluate("!!document.querySelector('#tesla-vin')"),'LuCI Tesla status check');
 await fetch(luci.origin+'/__fixture',{method:'POST',body:JSON.stringify({rpcErrors:{status:7}})});
 await b.evaluate("document.querySelector('#tesla-refresh').click()");
 await b.wait(()=>b.evaluate("document.querySelector('.cw-pill').textContent==='服务状态未知'"),'rejected status RPC stays unknown');
 checks.push('a rejected host-status RPC never claims that the service is running');
 assert.deepEqual(b.errors,[]);console.log(JSON.stringify({checks,shots,errors:b.errors},null,2));
} catch(e) {console.log(JSON.stringify({error:String(e),errors:b.errors,shot:await b.shot('luci-shell-failure')}));throw e;}
finally {await b.close();await luci.close();await service.close();}
