// Negative UX regressions with official LuCI, synthetic RPC/files and isolated Chrome only.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import {start} from './lib/ui-browser.mjs';
import {startFixture} from './lib/ui-fixture.mjs';
import {startLuciFixture} from './lib/luci-fixture.mjs';
import {luciConfirmation} from './lib/luci-confirmation.mjs';
if (!process.env.LUCI_RUNTIME_DIR) throw Error('LUCI_RUNTIME_DIR is required');
const api = await startFixture(), luci = await startLuciFixture(process.env.LUCI_RUNTIME_DIR, api.origin, {deviceAppend: process.env.LUCI_DEVICE_APPEND === '1'}), b = await start(), checks = [];
const calls = () => fetch(luci.origin + '/__calls').then(r => r.json());
const fixture = value => fetch(luci.origin + '/__fixture', {method: 'POST', body: JSON.stringify(value)});
const writes = async () => (await calls()).filter(c => c.object === 'uci' && ['set', 'apply', 'commit'].includes(c.method) || ['reload', 'network_prepare', 'network_commit'].includes(c.method)).length;
let openSequence = 0;
const open = async page => { await b.goto(luci.origin + '/luci?' + openSequence++ + '#' + page); await b.wait(() => b.evaluate("!!document.querySelector('.cw-tabs')"), 'LuCI loaded'); };
const click = text => b.evaluate(`(()=>{const e=[...document.querySelectorAll('button')].find(e=>e.offsetParent!==null&&e.textContent===${JSON.stringify(text)});if(!e||e.disabled)throw Error('button unavailable: '+${JSON.stringify(text)});e.click()})()`);
const dialog = () => b.wait(() => b.evaluate("document.body.classList.contains('modal-overlay-active') && !!document.querySelector('.cw-operation-confirmation')"), 'explicit confirmation');
const noDialog = () => b.wait(() => b.evaluate("!document.body.classList.contains('modal-overlay-active')"), 'cancelled confirmation');
const failures = [];
async function runningCase(name, run) { try { await run(); checks.push(name); } catch (e) { failures.push({name, error: String(e)}); } }
try {
    await api.fixture({mode: 'session', auth: true, phase: 'closed', open: false}); await b.viewport(1280, 900);
    await open('network');
    await b.evaluate("(async()=>{const v=await L.require('view.cowplay.main');window.__getter=0;const button=v.action('测试证书确认','cert_upload',()=>{window.__getter++;return ['synthetic-cert','synthetic-key']},true);v.root.append(button);button.click()})()");
    await dialog(); await luciConfirmation(b, 'cancel');
    assert.equal(await b.evaluate('window.__getter'), 0, 'cancel does not invoke a getter with busy side effects');
    await b.evaluate("[...document.querySelectorAll('button')].find(e=>e.textContent==='测试证书确认').click()"); await dialog();
    await b.call('Input.dispatchKeyEvent', {type: 'keyDown', key: 'Escape', code: 'Escape', windowsVirtualKeyCode: 27}); await noDialog();
    assert.equal(await b.evaluate('window.__getter'), 0);
    await b.evaluate("[...document.querySelectorAll('button')].find(e=>e.textContent==='测试证书确认').click()"); await dialog();
    await b.evaluate("(async()=>{const ui=await L.require('ui');ui.showModal('新的对话框',[E('button',{id:'replacement-focus',type:'button'},'继续')]);document.getElementById('replacement-focus').focus()})()");
    await b.wait(() => b.evaluate("(async()=>!(await L.require('view.cowplay.main')).pendingActions.size)()"), 'replaced confirmation settled');
    assert.equal(await b.evaluate('document.activeElement.id'), 'replacement-focus');
    await b.evaluate("(async()=>{(await L.require('ui')).hideModal()})()");
    checks.push('cancel, Escape and modal replacement settle without writing or stealing the new modal focus');

    await open('devices');
    await b.evaluate("(async()=>{const cp=await L.require('cowplay.rpc'),original=cp.call.bind(cp);window.__switchCalls=0;cp.call=(method,...args)=>method==='cmd'&&args[0]==='switchPhone'?(window.__switchCalls++,new Promise(resolve=>window.__releaseSwitch=()=>resolve({accepted:true}))):original(method,...args);})()");
    await click('切换'); await dialog();
    await b.evaluate("(async()=>{await(await L.require('view.cowplay.main')).refresh()})()");
    assert.equal(await b.evaluate("[...document.querySelectorAll('[data-page=devices] button')].some(e=>e.textContent==='等待确认…'&&e.disabled)"), true);
    await luciConfirmation(b); await b.wait(() => b.evaluate('window.__switchCalls===1'), 'one command');
    await b.evaluate("(async()=>{await(await L.require('view.cowplay.main')).refresh()})()");
    assert.equal(await b.evaluate("[...document.querySelectorAll('[data-page=devices] button')].some(e=>e.textContent==='处理中…'&&e.disabled)"), true);
    await b.evaluate("[...document.querySelectorAll('[data-page=devices] button')].find(e=>e.textContent==='处理中…').click()");
    assert.equal(await b.evaluate('window.__switchCalls'), 1); await b.evaluate('window.__releaseSwitch()');
    await b.wait(() => b.evaluate("(async()=>!(await L.require('view.cowplay.main')).pendingActions.size)()"), 'command lock released');
    checks.push('confirmation and request locks survive polling and prevent duplicate commands');

    await open('devices');
    await b.evaluate("(async()=>{const cp=await L.require('cowplay.rpc'),original=cp.call.bind(cp);cp.call=(method,...args)=>method==='cmd'&&args[0]==='forgetPhone'?new Promise((resolve,reject)=>window.__lateFailure=()=>reject(Error('旧页面合成失败'))):original(method,...args)})()");
    await click('忘记'); await luciConfirmation(b);
    await b.wait(() => b.evaluate("typeof window.__lateFailure==='function'"), 'pending old page command');
    await b.evaluate("(async()=>{(await L.require('view.cowplay.main')).select('overview');window.__lateFailure()})()");
    await b.wait(() => b.evaluate("(async()=>!(await L.require('view.cowplay.main')).pendingActions.size)()"), 'old request released');
    assert.equal(await b.evaluate("document.body.textContent.includes('旧页面合成失败')"), false);
    assert.equal(await b.evaluate('location.hash'), '#overview');
    checks.push('a late failure from an abandoned action does not report on the new page');

    await open('system');
    const selectFile = (text, name) => b.evaluate(`(()=>{const i=document.querySelector('input[aria-label="导入 CowPlay 配置"]'),d=new DataTransfer();d.items.add(new File([${JSON.stringify(text)}],${JSON.stringify(name)}));i.files=d.files;i.dispatchEvent(new Event('change',{bubbles:true}))})()`);
    await selectFile("config cowplay main\n option max_fps '30'\n", 'old.uci');
    await b.wait(() => b.evaluate("(async()=> !!(await L.require('view.cowplay.main')).importText)()"), 'first import read');
    await selectFile('x'.repeat(32769), 'oversized.uci');
    assert.equal(await b.evaluate("(async()=> (await L.require('view.cowplay.main')).importText)()"), '');
    assert.equal(await b.evaluate("[...document.querySelectorAll('button')].find(e=>e.textContent==='恢复配置').disabled"), true);
    await b.evaluate("window.__nativeRead=FileReader.prototype.readAsText;window.__reads=[];FileReader.prototype.readAsText=function(...args){window.__reads.push(()=>new Promise(resolve=>{this.addEventListener('loadend',resolve,{once:true});window.__nativeRead.apply(this,args)}))}");
    await selectFile('old pending import', 'A.uci'); await selectFile('new pending import', 'B.uci');
    await b.evaluate('window.__reads[1]()'); await b.evaluate('window.__reads[0]()');
    assert.equal(await b.evaluate("(async()=> (await L.require('view.cowplay.main')).importText)()"), 'new pending import');
    await b.evaluate('FileReader.prototype.readAsText=window.__nativeRead');
    await click('恢复配置'); await dialog(); await selectFile('replacement after confirmation', 'C.uci'); await noDialog();
    assert.equal((await calls()).filter(c => c.method === 'config_import').length, 0);
    checks.push('invalid files clear old imports; late reads and changed confirmation targets cannot restore stale content');

    await open('network');
    await b.evaluate("(()=>{const e=document.querySelector('input[id$=\".port\"]');e.value='19443';e.dispatchEvent(new Event('change',{bubbles:true}))})()");
    const before = await writes();
    await b.evaluate("(async()=>{window.__applyDone=false;window.__apply=(await L.require('view.cowplay.main')).handleSaveApply().finally(()=>window.__applyDone=true)})()");
    await dialog(); assert.equal(await writes(), before); await luciConfirmation(b, 'cancel'); await b.wait(() => b.evaluate('window.__applyDone'), 'cancel apply');
    assert.equal(await writes(), before); assert.equal(await b.evaluate("document.querySelector('input[id$=\".port\"]').value"), '19443');
    checks.push('ordinary apply confirms before any UCI/RPC writes and cancellation preserves the draft');

    await open('network'); await fixture({rpcErrors: {reload: 7}});
    await b.evaluate("(async()=>{const v=await L.require('view.cowplay.main'),ui=await L.require('ui');ui.changes.apply=()=>{document.dispatchEvent(new CustomEvent('uci-applied'))};window.__apply=v.handleSaveApply()})()");
    await luciConfirmation(b); await b.evaluate('window.__apply');
    await b.wait(() => b.evaluate("document.querySelector('.cw-footer').textContent.includes('重载失败')"), 'reload failure remains visible');
    assert.equal(await b.evaluate("document.querySelector('.cw-footer').textContent.includes('✓ 配置已应用')"), false); await fixture({rpcErrors: {}});
    checks.push('a failed core reload never reports applied success');

    await runningCase('hiding a pending token and concurrent reset/show replies respect their own display snapshots', async () => {
        await open('devices');
        await b.evaluate("(async()=>{const cp=await L.require('cowplay.rpc'),original=cp.call.bind(cp);window.__tokenReads=[];window.__tokenResets=[];cp.call=(method,...args)=>method==='token'?new Promise(resolve=>window.__tokenReads.push(value=>resolve({token:value}))):method==='token_reset'?new Promise(resolve=>window.__tokenResets.push(value=>resolve({token:value}))):original(method,...args)})()");
        await click('显示令牌'); await b.wait(() => b.evaluate('window.__tokenReads.length===1'), 'pending token read');
        await click('隐藏令牌'); await b.evaluate("window.__tokenReads[0]('synthetic-hidden-token')");
        await b.wait(() => b.evaluate("(async()=>!(await L.require('view.cowplay.main')).pendingActions.size)()"), 'hidden reply settled');
        assert.equal(await b.evaluate("document.querySelector('[data-page=devices] code').textContent"), '••••••');
        await click('显示令牌'); await b.wait(() => b.evaluate('window.__tokenReads.length===2'), 'older show request');
        await click('重置令牌'); await luciConfirmation(b); await b.wait(() => b.evaluate('window.__tokenResets.length===1'), 'newer reset request');
        await b.evaluate("window.__tokenReads[1]('synthetic-old-show');window.__tokenResets[0]('synthetic-new-reset')");
        await b.wait(() => b.evaluate("document.querySelector('[data-page=devices] code').textContent==='synthetic-new-reset'"), 'new reset owns display');
        await b.wait(() => b.evaluate("(async()=>!(await L.require('view.cowplay.main')).pendingActions.size)()"), 'first pair settled');
        await click('重置令牌'); await luciConfirmation(b); await b.wait(() => b.evaluate('window.__tokenResets.length===2'), 'older reset request');
        await click('显示令牌'); await b.wait(() => b.evaluate('window.__tokenReads.length===3'), 'newer show request');
        await b.evaluate("window.__tokenReads[2]('synthetic-new-show');window.__tokenResets[1]('synthetic-old-reset')");
        await b.wait(() => b.evaluate("(async()=>!(await L.require('view.cowplay.main')).pendingActions.size)()"), 'second pair settled');
        assert.equal(await b.evaluate("document.querySelector('[data-page=devices] code').textContent==='synthetic-new-show'"), true);
        await b.evaluate("(async()=>{const v=await L.require('view.cowplay.main');v.select('overview');v.select('devices')})()");
        assert.equal(await b.evaluate("document.querySelector('[data-page=devices] code').textContent"), '••••••', 'leaving and returning cannot retain a displayed token');
        await click('显示令牌'); await b.wait(() => b.evaluate('window.__tokenReads.length===4'), 'fresh token after return');
        await b.evaluate("window.__tokenReads[3]('synthetic-before-background')");
        await b.wait(() => b.evaluate("document.querySelector('[data-page=devices] code').textContent==='synthetic-before-background'"), 'displayed token before background');
        await b.evaluate("Object.defineProperty(document,'hidden',{configurable:true,value:true});document.dispatchEvent(new Event('visibilitychange'));delete document.hidden");
        assert.equal(await b.evaluate("document.querySelector('[data-page=devices] code').textContent"), '••••••', 'backgrounding hides a displayed token immediately');
    });
    await runningCase('an accepted import locks its file and its success still reloads the restored configuration', async () => {
        await open('system');
        await b.evaluate("(async()=>{const cp=await L.require('cowplay.rpc'),original=cp.call.bind(cp);cp.call=async(method,...args)=>{const result=await original(method,...args);if(method==='config_import')return new Promise(resolve=>window.__releaseImport=()=>resolve(result));return result;}})()");
        await selectFile("config cowplay main\n option max_fps '30'\n", 'accepted-A.uci');
        await b.wait(() => b.evaluate("[...document.querySelectorAll('button')].some(e=>e.textContent==='恢复配置'&&!e.disabled)"), 'current import read');
        await click('恢复配置'); await luciConfirmation(b); await b.wait(() => b.evaluate("typeof window.__releaseImport==='function'"), 'import running');
        assert.equal(await b.evaluate("document.querySelector('input[aria-label=\"导入 CowPlay 配置\"]').disabled"), true);
        assert.equal(await b.evaluate("document.body.classList.contains('modal-overlay-active') && !!document.querySelector('[data-cw-import-progress]') && document.querySelector('.cw').inert && document.querySelectorAll('#modal_overlay button').length===0"), true, 'running import blocks editing without a fake cancel');
        assert.equal(await b.evaluate("[...document.querySelectorAll('.cw-footer button')].every(b=>b.disabled)"),true,'restore also locks footer controls outside the inert view');
        const stagedBefore=(await calls()).filter(c=>c.object==='uci'&&c.method==='set').length;
        const blocked=await b.evaluate("(async()=>{const v=await L.require('view.cowplay.main');let saveError='';try{await v.handleSave()}catch(e){saveError=e.message}await v.handleReset();await v.handleSaveApply();return saveError})()");
        assert.match(blocked,/正在恢复/);assert.equal((await calls()).filter(c=>c.object==='uci'&&c.method==='set').length,stagedBefore,'no footer path can write over a running restore');

        await b.call('Input.dispatchKeyEvent', {type: 'keyDown', key: 'Escape', code: 'Escape', windowsVirtualKeyCode: 27});
        assert.equal(await b.evaluate("document.body.classList.contains('modal-overlay-active') && document.querySelector('.cw').inert"), true, 'Escape cannot pretend to cancel a committed import');
        await b.evaluate("document.querySelector('[data-target=devices]').click()");
        assert.equal(await b.evaluate('location.hash'), '#system', 'even a queued tab action cannot abandon the accepted import');
        await selectFile("config cowplay main\n option max_fps '60'\n", 'unaccepted-B.uci');
        assert.equal(await b.evaluate("(async()=> (await L.require('view.cowplay.main')).importText.includes(\"'30'\"))()"), true, 'a queued file event cannot disarm the accepted import');
        await b.evaluate('window.__releaseImport()');
        await b.wait(() => b.evaluate("typeof window.__releaseImport==='undefined' && document.querySelector('select[id$=\".max_fps\"]')?.value==='30'"), 'successful import reloads the accepted file');
        await open('system');
        await b.evaluate("(async()=>{const cp=await L.require('cowplay.rpc'),original=cp.call.bind(cp);cp.call=(method,...args)=>method==='config_import'?new Promise((resolve,reject)=>window.__rejectImport=()=>reject(Error('合成配置恢复失败'))):original(method,...args)})()");
        await selectFile("config cowplay main\n option max_fps '60'\n", 'retry.uci');
        await b.wait(() => b.evaluate("[...document.querySelectorAll('button')].some(e=>e.textContent==='恢复配置'&&!e.disabled)"), 'retry import ready');
        await click('恢复配置'); await luciConfirmation(b); await b.wait(() => b.evaluate("typeof window.__rejectImport==='function'"), 'retry import running');
        await b.evaluate('window.__rejectImport()');
        await b.wait(() => b.evaluate("!document.body.classList.contains('modal-overlay-active') && !document.querySelector('.cw').inert && !document.querySelector('input[aria-label=\"导入 CowPlay 配置\"]').disabled"), 'failed import closes progress and unlocks editing');
        assert.equal(await b.evaluate("document.body.textContent.includes('合成配置恢复失败')"), true);
    });
    await runningCase('saving DNS credential A locks the field and cannot clear a newer value B', async () => {
        await open('network');
        await b.evaluate("(async()=>{const v=await L.require('view.cowplay.main'),cp=await L.require('cowplay.rpc'),original=cp.call.bind(cp);v.renewSection.querySelector('details').open=true;cp.call=(method,...args)=>method==='ddns_token'?(window.__dnsSent=args[0],new Promise(resolve=>window.__releaseDns=()=>resolve({ok:true}))):original(method,...args);v.dnsToken.value='synthetic-A'})()");
        await click('保存证书 DNS 凭据'); await luciConfirmation(b); await b.wait(() => b.evaluate("typeof window.__releaseDns==='function'"), 'DNS write running');
        assert.equal(await b.evaluate("document.querySelector('input[aria-label=\"证书 DNS API 令牌\"]').disabled"), true);
        await b.evaluate("(async()=>{const v=await L.require('view.cowplay.main');v.dnsToken.value='synthetic-B';v.dnsToken.dispatchEvent(new Event('input',{bubbles:true}));window.__releaseDns()})()");
        await b.wait(() => b.evaluate("(async()=>!(await L.require('view.cowplay.main')).pendingActions.size)()"), 'DNS write settled');
        assert.equal(await b.evaluate("(async()=>{const v=await L.require('view.cowplay.main');return !v.dnsToken.disabled && v.dnsToken.value==='synthetic-B' && window.__dnsSent==='synthetic-A'})()"), true);
    });
    await runningCase('read ACL cannot reveal access tokens or whole system network configurations', async () => {
        const acl=JSON.parse(fs.readFileSync(new URL('../luci-app-cowplay/root/usr/share/rpcd/acl.d/luci-app-cowplay.json',import.meta.url),'utf8'))['luci-app-cowplay'];
        assert.equal(acl.read.ubus.cowplay.includes('token'),false);
        assert.equal(acl.write.ubus.cowplay.includes('token'),true);
        assert.deepEqual(acl.read.uci,['cowplay']);
        assert.equal(acl.read.ubus.rc,undefined);
    });
    await runningCase('refresh only requests the visible page data and ignores an abandoned page response', async () => {
        await open('overview'); await b.evaluate("L.Poll.stop()");
        const observed=await b.evaluate("(async()=>{const v=await L.require('view.cowplay.main'),cp=await L.require('cowplay.rpc'),original=cp.call.bind(cp),seen=[];cp.call=(method,...args)=>{seen.push(method);return original(method,...args)};try{await v.refresh();return seen;}finally{cp.call=original}})()");
        assert.deepEqual(observed,['status']);
        for(const [page,expected] of [['devices',['status','phones','screens']],['display',['status']],['network',['status','cert','network_info']],['system',['status','service_info','proc']]]) {
            const seen=await b.evaluate(`(async()=>{const v=await L.require('view.cowplay.main'),cp=await L.require('cowplay.rpc'),original=cp.call.bind(cp),seen=[];cp.call=(method,...args)=>{seen.push(method);return original(method,...args)};try{await v.select(${JSON.stringify(page)});return seen;}finally{cp.call=original}})()`);
            assert.deepEqual(seen,expected,page+' requests only its own data');
        }
        await b.evaluate("(async()=>await(await L.require('view.cowplay.main')).select('overview'))()");
        const late=await b.evaluate("(async()=>{const v=await L.require('view.cowplay.main'),cp=await L.require('cowplay.rpc'),original=cp.call.bind(cp),old=await original('status');let release,first=true;cp.call=(method,...args)=>method==='status'&&first?(first=false,new Promise(resolve=>release=()=>resolve({...old,version:'late-overview'}))):original(method,...args);try{const pending=v.refresh();v.select('display');v.data={...v.data,version:'current-display'};v.update();release();await pending;return v.data.version;}finally{cp.call=original}})()");
        assert.notEqual(late,'late-overview');
    });
    await runningCase('saving a stale LuCI form cannot replace a policy changed immediately elsewhere', async () => {
        await fixture({maxFps:'60',maxRes:'1080p'}); await open('display'); await b.evaluate("L.Poll.stop()");
        await b.evaluate("(()=>{const e=document.querySelector('select[id$=\".max_res\"]');e.value='2160p';e.dispatchEvent(new Event('change',{bubbles:true}))})()");
        await fixture({maxRes:'720p'}); const before=(await calls()).filter(c=>c.object==='uci'&&c.method==='set').length;
        const message=await b.evaluate("(async()=>{try{await(await L.require('view.cowplay.main')).handleSave();return '';}catch(e){return e.message}})()");
        assert.match(message,/已.*变化|已.*更新/);
        assert.equal((await calls()).filter(c=>c.object==='uci'&&c.method==='set').length,before);
        assert.equal(await b.evaluate("document.querySelector('.cw-footer').textContent.includes('刷新')"),true);
    });
    await runningCase('read-only roles render every page with denied token access and disabled write controls', async () => {
        const readonly=await startLuciFixture(process.env.LUCI_RUNTIME_DIR,api.origin,{deviceAppend:process.env.LUCI_DEVICE_APPEND==='1',readOnly:true});
        try {
            await b.goto(readonly.origin+'/luci#devices'); await b.wait(()=>b.evaluate("!!document.querySelector('.cw-tabs')"),'read-only LuCI view');
            for(const page of ['overview','devices','display','network','checks','info','timeline','logs','proc','system'])await b.evaluate(`(async()=>{const v=await L.require('view.cowplay.main');v.select(${JSON.stringify(page)});await v.refresh()})()`);
            assert.equal(await b.evaluate("[...document.querySelectorAll('button[data-cw-action],.cw-footer button,input[type=file],textarea,input[type=password],.cbi-map select')].every(e=>e.disabled)"),true);
            assert.equal((await fetch(readonly.origin+'/__calls').then(r=>r.json())).some(c=>c.method==='token'),false);
            assert.equal(await b.evaluate("(async()=>{try{await(await L.require('cowplay.rpc')).call('token');return false;}catch{return true;}})()"),true);
        } finally { await readonly.close(); }
    });
    console.log(JSON.stringify({runtime: process.env.LUCI_DEVICE_APPEND === '1' ? 'observed append replay' : 'pinned upstream', checks, failures, errors: b.errors}, null, 2));
    assert.deepEqual(failures, []); assert.deepEqual(b.errors, []);
} catch (error) { console.log(JSON.stringify({error: String(error), checks, errors: b.errors, shot: await b.shot('luci-operation-ux-failure')})); throw error; }
finally { await b.close(); await luci.close(); await api.close(); }
