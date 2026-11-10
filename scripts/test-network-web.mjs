// Port configuration and concise default UI, through real LuCI with synthetic RPC.
import assert from 'node:assert/strict';
import {start} from './lib/ui-browser.mjs';
import {startFixture} from './lib/ui-fixture.mjs';
import {startLuciFixture} from './lib/luci-fixture.mjs';

if (!process.env.LUCI_RUNTIME_DIR) throw Error('LUCI_RUNTIME_DIR is required');
const api = await startFixture();
const luci = await startLuciFixture(process.env.LUCI_RUNTIME_DIR, api.origin, {deviceAppend: process.env.LUCI_DEVICE_APPEND === '1'});
const b = await start(), checks = [], failures = [], shots = [];
const fixture = value => fetch(luci.origin + '/__fixture', {method: 'POST', body: JSON.stringify(value)});
const calls = () => fetch(luci.origin + '/__calls').then(r => r.json());
const writes = async () => (await calls()).filter(c => c.object === 'uci' && c.method === 'set');
const changePort = value => b.evaluate(`(()=>{const e=document.querySelector('input[id$=".port"]');e.value=${JSON.stringify(value)};e.dispatchEvent(new Event('change',{bubbles:true}));})()`);
const save = () => b.evaluate("(async()=>{try{await (await L.require('view.cowplay.main')).handleSave();return {ok:true}}catch(e){return {ok:false,error:e.message}}})()");
const open = async name => {
    await b.goto(luci.origin + '/luci?' + name + '#network');
    await b.wait(() => b.evaluate("!!document.querySelector('input[id$=\".port\"]')"), 'network form');
};
const test = async (name, run) => {
    try { await run(); checks.push(name); console.log('PASS: ' + name); }
    catch (e) { failures.push({name, error: String(e)}); console.log('FAIL: ' + name + ': ' + e); }
};
const click = text => b.evaluate(`(()=>{const e=[...document.querySelectorAll('button')].find(e=>e.offsetParent!==null&&getComputedStyle(e).visibility!=='hidden'&&e.textContent.trim()===${JSON.stringify(text)});if(!e||e.disabled)throw Error('按钮不可用');e.click()})()`);
const apply = () => b.evaluate("(async()=>{const v=await L.require('view.cowplay.main');window.__networkApplyDone=false;window.__networkApply=v.handleSaveApply(null,'0').finally(()=>window.__networkApplyDone=true)})()");

try {
    await b.viewport(1440, 1100);
    await test('occupied 443 can be saved and the real owner is displayed without stopping it', async () => {
        await open('port');
        await changePort('443');
        await fixture({portBusy: true});
        const before = (await writes()).length;
        const conflict = await save();
        assert.equal(conflict.ok, true);
        assert.ok((await writes()).length > before);
        assert.equal(await b.evaluate("document.querySelector('input[id$=\".port\"]').value"), '443');
        assert.match(await b.evaluate("(async()=>{const v=await L.require('view.cowplay.main');return v.portStatus.innerText})()"), /LuCI \/ uhttpd.*4242/);
        shots.push(await b.shot('network-occupied-443'));
        assert.equal((await calls()).some(c=>['network_prepare','network_commit','service'].includes(c.method)),false);
        await fixture({portBusy: false});
        assert.equal((await save()).ok, true);
        assert.equal((await writes()).some(c=>c.args.values.port==='443'),true);
        assert.equal((await calls()).some(c => c.method === 'service'), false, 'saving never restarts a service');
        assert.equal(await b.evaluate("document.querySelector('input[id$=\".port\"]').value"), '443');
    });
    await test('invalid ports are rejected even when CowPlay is unavailable', async () => {
        await fixture({failStatus: true});
        await b.evaluate("(async()=>{await (await L.require('view.cowplay.main')).refresh()})()");
        const before = (await writes()).length;
        for (const value of ['0', '65536', '443.5']) {
            await changePort(value);
            assert.equal((await save()).ok, false, value + ' must not be persisted');
        }
        assert.equal((await writes()).length, before);
        await changePort('18443');
        assert.equal((await save()).ok, true, 'valid default port still works while stopped');
        await fixture({failStatus: false});
    });
    await test('hotspot editing is absent and manual certificates are separate from DNS issuance', async () => {
        await open('configuration-relations');
        assert.equal(await b.evaluate("document.querySelectorAll('select[id$=\".wifi_iface\"],input[id$=\".ssid\"],input[id$=\".key\"]').length"),0);
        const relation=await b.evaluate("(async()=>{const v=await L.require('view.cowplay.main'),uci=await L.require('uci');return {manual:v.certSection.textContent,renewal:v.renewSection.textContent,manualFiles:v.certSection.querySelectorAll('input[type=file]').length,renewalFiles:v.renewSection.querySelectorAll('input[type=file]').length,iface:uci.get('cowplay','main','wifi_iface')}})()");
        assert.doesNotMatch(relation.manual,/DNS|challenge/);assert.match(relation.renewal,/DNS-01/);assert.equal(relation.manualFiles,2);assert.equal(relation.renewalFiles,0);assert.equal(relation.iface,'default_radio1');
    });
    await test('cancelling takeover preserves the unsaved draft without writes', async () => {
        await fixture({portBusy:true,takeoverReset:true});await open('cancel-takeover');await changePort('443');const before=(await calls()).length;await apply();
        await b.wait(()=>b.evaluate("document.getElementById('modal_overlay')?.innerText.includes('覆盖端口 443')"),'takeover confirmation');
        assert.equal((await calls()).slice(before).some(c=>c.method==='network_prepare'),false);
        await click('取消');await b.wait(()=>b.evaluate('window.__networkApplyDone'),'cancel completed');
        assert.equal((await calls()).slice(before).some(c=>c.object==='uci'&&['set','apply'].includes(c.method)||['network_prepare','network_commit','service'].includes(c.method)),false);
        assert.equal(await b.evaluate("document.querySelector('input[id$=\".port\"]').value"),'443','cancel preserves the draft');
    });
    await test('confirmed takeover applies first, confirms management, and reports verified success', async () => {
        await fixture({portBusy:true,takeoverReset:true,takeoverOutcome:'complete'});await open('complete-takeover');await changePort('443');const before=(await calls()).length;const previous=await b.evaluate('location.href');await apply();
        await b.wait(()=>b.evaluate("document.getElementById('modal_overlay')?.innerText.includes('覆盖端口 443')"),'takeover confirmation');
        assert.match(await b.evaluate("document.getElementById('modal_overlay').innerText"),/uhttpd.*4242/s);
        await b.wait(()=>b.evaluate("getComputedStyle(document.getElementById('modal_overlay')).opacity==='1'"),'visible confirmation');
        shots.push(await b.shot('network-takeover-confirmation'));
        await b.viewport(390,844);
        assert.equal(await b.evaluate("document.documentElement.scrollWidth>innerWidth"),false);
        assert.equal(await b.evaluate("[...document.querySelectorAll('#modal_overlay button')].every(b=>{const r=b.getBoundingClientRect();return r.left>=0&&r.right<=innerWidth&&r.top>=0&&r.bottom<=innerHeight})"),true);
        shots.push(await b.shot('network-takeover-confirmation-390'));
        await b.viewport(1440,1100);
        await click('覆盖并应用');await b.wait(()=>b.evaluate('window.__networkApplyDone'),'takeover completed');
        const sequence=(await calls()).slice(before);const index=method=>sequence.findIndex(c=>c.method===method);
        assert.ok(index('network_prepare')>=0&&index('apply')>index('network_prepare')&&index('network_commit')>index('apply')&&index('network_confirm')>index('network_commit'));
        const after=new URL(await b.evaluate('location.href')),original=new URL(previous);assert.equal(after.origin,original.origin);assert.equal(after.pathname,original.pathname);assert.equal(after.hash,original.hash);assert.deepEqual([...after.searchParams],[...original.searchParams],'the independent HTTP management page and query values are preserved');
        assert.equal(await b.evaluate("document.body.innerText.includes('CowPlay 已接管 443，HTTPS 验证通过')"),true);
        assert.equal(await b.evaluate("(async()=>{const v=await L.require('view.cowplay.main');return v.portStatus.innerText})()"),'','verified success refreshes the owner notice');
        assert.equal(sequence.some(c=>c.method==='service'),false,'the browser never directly stops a service');
    });
    await test('takeover failure and commit rejection do not claim success', async () => {
        for(const rejected of [false,true]){
            await fixture({portBusy:true,takeoverReset:true,takeoverOutcome:'rolled_back',takeoverCommitError:rejected?'合成提交拒绝':null});await open('failed-takeover-'+rejected);await changePort('443');await apply();
            await b.wait(()=>b.evaluate("document.getElementById('modal_overlay')?.innerText.includes('覆盖端口 443')"),'takeover confirmation');await click('覆盖并应用');await b.wait(()=>b.evaluate('window.__networkApplyDone'),'failure completes without long wait');
            assert.equal(await b.evaluate("document.getElementById('modal_overlay').innerText.includes('接管未完成')"),true);
            if(rejected)assert.match(await b.evaluate("document.getElementById('modal_overlay').innerText"),/合成提交拒绝/);
            await click('关闭');
        }
        await fixture({portBusy:false,takeoverReset:true,takeoverCommitError:null});
    });
    await test('legacy LuCI cannot start takeover before configuration confirmation', async () => {
        await fixture({portBusy:true,takeoverReset:true,takeoverOutcome:'complete'});
        await open('legacy-uci-apply');await changePort('443');
        // Observed F50 LuCI: apply() resolves before its delayed confirm callback.
        await b.evaluate("(async()=>{const u=await L.require('uci');u.apply=function(timeout){return u.callApply(timeout||10,true).then(rv=>{if(rv!==0)throw Error('apply failed');setTimeout(()=>u.callConfirm(),1000)})}})()");
        const before=(await calls()).length;await apply();
        await b.wait(()=>b.evaluate("document.getElementById('modal_overlay')?.innerText.includes('覆盖端口 443')"),'legacy takeover confirmation');
        await click('覆盖并应用');await b.wait(()=>b.evaluate('window.__networkApplyDone'),'legacy takeover completed');
        const sequence=(await calls()).slice(before),index=(object,method)=>sequence.findIndex(c=>c.object===object&&c.method===method);
        assert.ok(index('uci','apply')>=0&&index('uci','confirm')>index('uci','apply')&&index('cowplay','network_commit')>index('uci','confirm'),'UCI confirmation must complete before the old management page can navigate away');
    });
    await test('failed UCI confirmation cancels preparation without restarting services', async () => {
        await fixture({portBusy:true,takeoverReset:true,takeoverOutcome:'complete'});
        await open('uci-confirm-failure');await changePort('443');
        await b.evaluate("(async()=>{const u=await L.require('uci');u.callConfirm=()=>Promise.reject(Error('合成确认失败'))})()");
        const before=(await calls()).length;await apply();
        await b.wait(()=>b.evaluate("document.getElementById('modal_overlay')?.innerText.includes('覆盖端口 443')"),'confirm-failure modal');
        await click('覆盖并应用');await b.wait(()=>b.evaluate('window.__networkApplyDone'),'confirm failure returns');
        const sequence=(await calls()).slice(before);
        assert.equal(sequence.some(c=>c.method==='network_cancel'),true);
        assert.equal(sequence.some(c=>['network_commit','network_confirm','service'].includes(c.method)),false);
        assert.equal(await b.evaluate("document.body.innerText.includes('配置应用失败，未接管端口')"),true);
    });
    await test('late inspection failure cannot replace the newer occupied-port report', async () => {
        await open('late-check');
        const result=await b.evaluate("(async()=>{const v=await L.require('view.cowplay.main'),cp=await L.require('cowplay.rpc'),net=await L.require('cowplay.network'),original=cp.call;let reject;cp.call=function(method,port){if(method==='network_check')return port===9443?new Promise((r,j)=>reject=j):Promise.resolve({ok:true,port:443,available:false,owners:[],can_takeover:true});return original.apply(this,arguments)};try{const first=net.inspect(v,9443);await net.inspect(v,443);reject(Error('old inspection failed'));await first;return {port:v.portReport.port,available:v.portReport.available}}finally{cp.call=original}})()");
        assert.deepEqual(result,{port:443,available:false});
        const urls=await b.evaluate("(async()=>{const net=await L.require('cowplay.network'),job={id:'takeover-synthetic',management_scheme:'https',management_port:8443,original_management_port:443};const url=net.managementURL(job,'https://router.example.test/cgi-bin/luci/admin/services/cowplay?x=1#network');let rejected=false;try{net.managementURL({...job,management_scheme:'http'},'https://router.example.test/cgi-bin/luci/admin/services/cowplay')}catch(e){rejected=true}return {url,rejected}})()");
        assert.match(urls.url,/^https:\/\/router\.example\.test:8443\/cgi-bin\/luci\/admin\/services\/cowplay\?/);assert.equal(urls.rejected,true);
    });
    await test('changing the input during apply inspection cannot apply a different saved port', async () => {
        await open('apply-inspection-race');
        const result=await b.evaluate("(async()=>{const v=await L.require('view.cowplay.main'),cp=await L.require('cowplay.rpc'),ui=await L.require('ui'),net=await L.require('cowplay.network'),uci=await L.require('uci');const original={save:v.handleSave,form:v.portOption.formvalue,call:cp.call,apply:ui.changes.apply,takeover:net.apply,port:uci.get('cowplay','main','port')};let value='443',release,started,ordinary=0,takeover=0;const ready=new Promise(r=>started=r);v.handleSave=()=>Promise.resolve();v.portOption.formvalue=()=>value;uci.set('cowplay','main','port','443');cp.call=function(method,port){if(method==='network_check')return new Promise(resolve=>{release=resolve;started()});return original.call.apply(this,arguments)};ui.changes.apply=()=>ordinary++;net.apply=()=>takeover++;try{const pending=v.handleSaveApply(null,'0');await ready;value='9443';v.portReport={port:9443,available:true};release({ok:true,port:443,available:false,can_takeover:true});await pending;return {ordinary,takeover,expired:document.body.innerText.includes('端口检查已过期')}}finally{v.handleSave=original.save;v.portOption.formvalue=original.form;cp.call=original.call;ui.changes.apply=original.apply;net.apply=original.takeover;uci.set('cowplay','main','port',original.port)}})()");
        assert.deepEqual(result,{ordinary:0,takeover:0,expired:true});
    });
    await test('port settings are prominent and explanations stay concise by default', async () => {
        await open('concise');
        for (const theme of ['bootstrap', 'material']) {
            await b.evaluate(`new Promise((resolve,reject)=>{const l=document.getElementById('luci-theme');if(l.getAttribute('href')==='/themes/${theme}.css')return resolve(true);l.onload=()=>resolve(true);l.onerror=()=>reject(Error('theme missing'));l.href='/themes/${theme}.css'})`);
            for (const [width, height] of [[1440, 1100], [390, 844]]) {
                await b.viewport(width, height);
                await b.evaluate('window.scrollTo(0,0)');
                const layout = await b.evaluate("(()=>{const page=document.querySelector('[data-page=network]'),port=page.querySelector('input[id$=\".port\"]'),r=port.getBoundingClientRect();return {title:page.querySelector('h3').textContent,portVisible:r.top>=0&&r.bottom<=innerHeight,overflow:document.documentElement.scrollWidth>innerWidth,explanations:[...page.querySelectorAll('p,.cbi-value-description')].filter(e=>e.getClientRects().length).map(e=>e.innerText.replace(/\\s/g,'')).join('').length,helpClosed:[...page.querySelectorAll('details')].every(e=>!e.open)}})()");
                console.log('LAYOUT: ' + JSON.stringify({theme, width, ...layout}));
                assert.equal(layout.title, '访问地址与端口');
                assert.equal(layout.portVisible, true, 'HTTPS port is visible without scrolling');
                assert.equal(layout.overflow, false);
                assert.equal(layout.helpClosed, true);
                assert.ok(layout.explanations <= 300, 'default explanations: ' + layout.explanations);
                shots.push(await b.shot(`network-${theme}-${width}`));
            }
        }
        await b.evaluate("document.querySelector('[data-cert-details]').open=true");
        await b.evaluate("(async()=>{await (await L.require('view.cowplay.main')).refresh()})()");
        assert.equal(await b.evaluate("document.querySelector('[data-cert-details]').open"), true, 'polling preserves expanded details');
    });
    await test('certificate errors remain visible with details collapsed', async () => {
        await open('visible-errors');
        const result = await b.evaluate("(async()=>{const v=await L.require('view.cowplay.main');v.certData.error='合成读取失败';v.certData.task={cert_ok:false,cert_result:'合成 DNS 授权失败'};v.updateCert();return {closed:!document.querySelector('[data-cert-details]').open,text:v.certInfo.innerText}})()");
        assert.equal(result.closed, true);
        assert.match(result.text, /合成读取失败/);
        assert.match(result.text, /合成 DNS 授权失败/);
    });
    console.log(JSON.stringify({runtime: process.env.LUCI_DEVICE_APPEND === '1' ? 'observed append replay' : 'pinned upstream', checks, failures, errors: b.errors, shots}, null, 2));
    assert.deepEqual(failures, []);
    assert.deepEqual(b.errors, []);
} finally { await b.close(); await luci.close(); await api.close(); }
