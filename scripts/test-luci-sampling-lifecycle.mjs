import assert from 'node:assert/strict';
import {start} from './lib/ui-browser.mjs';
import {startFixture} from './lib/ui-fixture.mjs';
import {startLuciFixture} from './lib/luci-fixture.mjs';

const api = await startFixture();
const luci = await startLuciFixture(process.env.LUCI_RUNTIME_DIR, api.origin);
const b = await start(), checks = [], failures = [];
const run = async (name, fn) => { try { await fn(); checks.push(name); console.log('PASS:', name); } catch (e) { failures.push({name, error: String(e)}); console.log('FAIL:', name, String(e)); } };
const settled = () => b.wait(() => b.evaluate("(async()=>{const v=await L.require('view.cowplay.main');return !v.diagPending&&!v.refreshPending})()"), 'settled');
const count = async () => (await fetch(luci.origin + '/__calls').then(r => r.json())).filter(c => c.method === 'diag').length;
const resume = async () => { await b.evaluate("(async()=>{const v=await L.require('view.cowplay.main');v.sampling=true;await v.loadDiag()})()"); await settled(); };
try {
  await api.fixture({mode: 'session', auth: true});
  await b.goto(luci.origin + '/luci#checks');
  await b.wait(() => b.evaluate("!!document.querySelector('.cw-measurements')"), 'initial sample');
  await settled();
  await run('leaving real-time stops sampling and returning needs an explicit restart', async () => {
    await b.evaluate("(async()=>{const v=await L.require('view.cowplay.main');await v.select('overview')})()");
    const before = await count();
    await b.evaluate("(async()=>{const v=await L.require('view.cowplay.main');await v.refresh();await v.select('checks')})()");
    await settled();
    assert.equal(await b.evaluate("(async()=>{const v=await L.require('view.cowplay.main');return v.sampling})()"), false);
    assert.equal(await count(), before);
    assert.ok(await b.evaluate("document.querySelector('[data-page=diag]').textContent.includes('开始采样')"));
  });
  await run('a paused view stays paused after the connection context changes', async () => {
    await b.evaluate("(async()=>{const v=await L.require('view.cowplay.main');v.sampling=false;v.lastSample=null;await v.loadDiag()})()");
    const before = await count();
    await b.evaluate("(async()=>{const v=await L.require('view.cowplay.main');v.data.context={session:'new-synthetic-session'};v.update();if(v.diagPending)await v.diagPending.promise})()");
    assert.equal(await count(), before);
    assert.equal(await b.evaluate("document.querySelectorAll('[data-page=diag] .cw-reading').length"), 0);
  });
  await run('hiding the page stops sampling and discards a late diagnostic reply', async () => {
    await resume();
    const result = await b.evaluate("(async()=>{const v=await L.require('view.cowplay.main'),cp=await L.require('cowplay.rpc'),call=cp.call.bind(cp);const reply=await call('diag');reply.stats.server.frames_in=987654321;let release;cp.call=(m,...a)=>m==='diag'?new Promise(r=>release=()=>r(reply)):call(m,...a);try{const pending=v.loadDiag();Object.defineProperty(document,'hidden',{configurable:true,value:true});document.dispatchEvent(new Event('visibilitychange'));release();await pending;const stopped=!v.sampling,stale=v.lastSample?.stats?.server?.frames_in===987654321;cp.call=call;delete document.hidden;document.dispatchEvent(new Event('visibilitychange'));await v.loadDiag();return {stopped,stale};}finally{delete document.hidden;cp.call=call;}})()");
    assert.deepEqual(result, {stopped: true, stale: false});
  });
  await run('pagehide stops sampling, including history cache navigation', async () => {
    await resume();
    await b.evaluate("window.dispatchEvent(new PageTransitionEvent('pagehide',{persisted:true}))");
    assert.equal(await b.evaluate("(async()=>{const v=await L.require('view.cowplay.main');return v.sampling})()"), false);
    const before = await count();
    await b.evaluate("window.dispatchEvent(new PageTransitionEvent('pageshow',{persisted:true}))");
    await b.evaluate("(async()=>{const v=await L.require('view.cowplay.main');await v.refresh();if(v.diagPending)await v.diagPending.promise})()");
    assert.equal(await count(), before);
  });
  await run('real-time no longer contains duplicate check and export actions', async () => {
    assert.equal(await b.evaluate("/连接检查与导出|开始检查|下载诊断包/.test(document.querySelector('[data-page=diag]').textContent)"), false);
  });
  assert.deepEqual(b.errors, []);
  console.log(JSON.stringify({checks, failures, errors: b.errors}, null, 2));
  assert.deepEqual(failures, []);
} finally { await b.close(); await luci.close(); await api.close(); }
