import {luciConfirmation} from './lib/luci-confirmation.mjs';
// Real LuCI DOM and RPC with synthetic PEM content only; never contacts an F50.
import assert from 'node:assert/strict';
import {start} from './lib/ui-browser.mjs';
import {startFixture} from './lib/ui-fixture.mjs';
import {startLuciFixture} from './lib/luci-fixture.mjs';

if (!process.env.LUCI_RUNTIME_DIR) throw Error('LUCI_RUNTIME_DIR is required');
const api = await startFixture();
const luci = await startLuciFixture(process.env.LUCI_RUNTIME_DIR, api.origin, {deviceAppend: process.env.LUCI_DEVICE_APPEND === '1'});
const b = await start(), checks = [], failures = [], shots = [];
const cert = '-----BEGIN CERTIFICATE-----\nsynthetic-certificate-only\n-----END CERTIFICATE-----';
const key = '-----BEGIN PRIVATE KEY-----\nsynthetic-private-key-only\n-----END PRIVATE KEY-----';
const calls = async () => fetch(luci.origin + '/__calls').then(r => r.json());
const uploads = async () => (await calls()).filter(c => c.method === 'cert_upload');
const button = "[...document.querySelectorAll('button')].find(e=>/^(验证并替换证书|再点一次确认验证并替换证书|处理中…)$/u.test(e.textContent))";
const enabled = () => b.evaluate(`!!(${button})&&!(${button}).disabled`);
const notice = () => b.evaluate("document.querySelector('.cw-cert-notice')?.textContent || ''");
const paste = (which, value) => b.evaluate(`(()=>{const e=document.querySelector('textarea[aria-label="${which} PEM 内容"]');if(!e)throw Error('缺少 ${which} PEM 粘贴入口');e.value=${JSON.stringify(value)};e.dispatchEvent(new Event('input',{bubbles:true}));})()`);
const file = (which, value) => b.evaluate(`(()=>{const e=document.querySelector('input[aria-label="${which} PEM"]'),d=new DataTransfer();d.items.add(new File([${JSON.stringify(value)}],'synthetic.pem'));e.files=d.files;e.dispatchEvent(new Event('change',{bubbles:true}));})()`);
const click = () => b.evaluate(`(()=>{const e=${button};if(!e||e.disabled)throw Error('替换按钮不可用');e.click();})()`);
const open = async name => {
    await b.goto(luci.origin + '/luci?' + name + '#network');
    await b.wait(() => b.evaluate("!!document.querySelector('[data-page=network] input[aria-label=\"证书链 PEM\"]')"), 'certificate page');
};
const test = async (name, run) => {
    try { await run(); checks.push(name); console.log('PASS: ' + name); }
    catch (e) { failures.push({name, error: String(e)}); console.log('FAIL: ' + name + ': ' + e); }
};

try {
    await b.viewport(1440, 1100);
    await test('PEM can be pasted, edited, confirmed and submitted without choosing files', async () => {
        await open('paste');
        assert.equal(await enabled(), false);
        await paste('证书链', cert);
        assert.equal(await enabled(), false, 'missing key blocks replacement');
        await paste('私钥', key);
        assert.equal(await enabled(), true);
        assert.equal(await b.evaluate("document.querySelector('.cw-footer').textContent.includes('有更改尚未保存')"), false, 'PEM edits are not unsaved UCI changes');
        const before = (await uploads()).length;
        await click();
        assert.equal((await uploads()).length, before, 'first click only confirms');
        await paste('证书链', cert.replace('certificate-only', 'edited-certificate-only'));
        assert.equal(await b.evaluate(`(${button}).textContent`), '验证并替换证书', 'editing cancels previous confirmation');
        await click();
        assert.equal((await uploads()).length, before);
        await luciConfirmation(b);
        await b.wait(async () => (await uploads()).length === before + 1, 'pasted PEM RPC');
        const sent = (await uploads()).at(-1).args;
        assert.equal(sent.cert, cert.replace('certificate-only', 'edited-certificate-only'));
        assert.equal(sent.key, key);
        await b.wait(() => b.evaluate("[...document.querySelectorAll('[data-page=network] textarea')].every(e=>e.value==='')"), 'success clears PEM');
        assert.match(await notice(), /热加载|立即生效/);
        assert.equal((await calls()).some(c => c.method === 'service'), false, 'upload does not restart services');
        assert.equal(await b.evaluate("document.querySelector('.cw-footer').textContent.includes('有更改尚未保存')"), false, 'immediate certificate replacement needs no UCI save');
    });
    await test('file upload works without Blob.text and loads editable PEM fields', async () => {
        await open('legacy-file');
        await b.evaluate('Blob.prototype.text=undefined');
        await file('证书链', cert); await file('私钥', key);
        await b.wait(enabled, 'file selection without Blob.text');
        assert.deepEqual(await b.evaluate("[...document.querySelectorAll('[data-page=network] textarea')].map(e=>e.value)"), [cert, key]);
        await click(); await luciConfirmation(b);
        await b.wait(() => b.evaluate("[...document.querySelectorAll('[data-page=network] textarea')].every(e=>e.value==='')"), 'uploaded file contents cleared');
    });
    await test('invalid and oversized replacement cannot submit stale PEM', async () => {
        await open('validation');
        await paste('证书链', cert); await paste('私钥', key);
        await file('证书链', 'invalid');
        await b.wait(async () => /证书链须为完整 PEM/.test(await notice()), 'invalid file read finishes with a format error');
        assert.equal(await enabled(), false);
        await paste('证书链', cert);
        await paste('私钥', '-----BEGIN ENCRYPTED PRIVATE KEY-----\nsynthetic\n-----END ENCRYPTED PRIVATE KEY-----');
        assert.equal(await enabled(), false);
        assert.match(await notice(), /加密/);
        await paste('私钥', key);
        await paste('证书链', cert.replace('synthetic-certificate-only', 'synthetic' + '\n'.repeat(25000)));
        assert.equal(await enabled(), false, 'JSON escaped bytes, not raw text length, enforce the limit');
        assert.match(await notice(), /48 KiB/);
        await paste('证书链', cert);
        assert.equal(await enabled(), true, 'valid edit recovers without reload');
    });
    await test('backend rejection stays visible and allows corrected mixed file/paste retry', async () => {
        await open('rejection');
        await file('证书链', cert); await paste('私钥', key);
        await b.wait(enabled, 'mixed inputs');
        await fetch(luci.origin + '/__fixture', {method: 'POST', body: JSON.stringify({rpcErrors: {cert_upload: 7}})});
        await click(); await luciConfirmation(b);
        await b.wait(async () => /未完成|失败/.test(await notice()), 'inline RPC error');
        assert.equal(await enabled(), true, 'rejected submission is retryable');
        assert.equal(await b.evaluate("document.querySelector('textarea[aria-label=\"私钥 PEM 内容\"]').value"), key);
        await fetch(luci.origin + '/__fixture', {method: 'POST', body: JSON.stringify({rpcErrors: {}})});
        await click(); await luciConfirmation(b);
        await b.wait(() => b.evaluate(`document.querySelector('textarea[aria-label="私钥 PEM 内容"]').value === ''`), 'retry clears private key');
    });
    await test('certificate replacement preserves unrelated unsaved UCI edits', async () => {
        await open('unsaved-config');
        await b.evaluate("(()=>{const e=document.querySelector('input[id$=\".port\"]');e.value='19443';e.dispatchEvent(new Event('input',{bubbles:true}));})()");
        assert.equal(await b.evaluate("document.querySelector('.cw-footer').textContent.includes('有更改尚未保存')"), true);
        await paste('证书链', cert); await paste('私钥', key);
        await click(); await luciConfirmation(b);
        await b.wait(() => b.evaluate(`document.querySelector('textarea[aria-label="私钥 PEM 内容"]').value === ''`), 'replacement completed with unsaved UCI');
        assert.equal(await b.evaluate("document.querySelector('.cw-footer').textContent.includes('有更改尚未保存')"), true);
        assert.equal(await b.evaluate("document.querySelector('input[id$=\".port\"]').value"), '19443');
    });
    await test('late file reads cannot overwrite newer pasted PEM and read failures recover', async () => {
        await open('file-race');
        await b.evaluate("window.__readAsText=FileReader.prototype.readAsText;window.__pendingReads=[];FileReader.prototype.readAsText=function(...args){window.__pendingReads.push(()=>new Promise(resolve=>{this.addEventListener('loadend',()=>resolve(true),{once:true});window.__readAsText.apply(this,args)}))}");
        await file('证书链', cert.replace('certificate-only', 'old-file-only'));
        await paste('证书链', cert); await paste('私钥', key);
        await b.evaluate("FileReader.prototype.readAsText=window.__readAsText;Promise.all(window.__pendingReads.map(read=>read()))");
        assert.equal(await b.evaluate(`document.querySelector('textarea[aria-label="证书链 PEM 内容"]').value`), cert);
        assert.equal(await enabled(), true);
        await b.evaluate("FileReader.prototype.readAsText=function(){throw Error('synthetic read failure')}");
        await file('证书链', cert);
        assert.equal(await enabled(), false);
        assert.match(await notice(), /无法读取/);
        await paste('证书链', cert);
        assert.equal(await enabled(), true, 'paste recovers after file read failure');
    });
    await test('PEM exported with OpenSSL bag attributes works through files and paste', async () => {
        await open('pem-metadata');
        const exported = 'Bag Attributes\n    friendlyName: synthetic-only\n' + key + '\n';
        await paste('证书链', cert); await paste('私钥', exported);
        assert.equal(await enabled(), true, 'valid PEM with export metadata remains supported');
        await file('私钥', exported);
        await b.wait(enabled, 'PEM file with bag attributes');
        const before = (await uploads()).length;
        await click(); await luciConfirmation(b);
        await b.wait(async () => (await uploads()).length === before + 1, 'PEM export uploaded');
        assert.equal((await uploads()).at(-1).args.key, exported.trim());
    });
    await test('a running certificate task disables upload with a reason and then permits retry', async () => {
        await open('certificate-job');
        await paste('证书链', cert); await paste('私钥', key);
        await b.evaluate("(async()=>{const v=await L.require('view.cowplay.main');v.certData.task={running:true};v.updateCert()})()");
        assert.equal(await enabled(), false);
        assert.match(await notice(), /证书任务正在进行/);
        await b.evaluate("(async()=>{const v=await L.require('view.cowplay.main');v.certData.task={running:false};v.updateCert()})()");
        assert.equal(await enabled(), true);
        assert.match(await notice(), /已就绪/);
    });
    await test('desktop and mobile editors remain within the viewport in both LuCI themes', async () => {
        await open('layout');
        for (const theme of ['bootstrap', 'material']) {
            await b.evaluate(`new Promise((resolve,reject)=>{const l=document.getElementById('luci-theme');if(l.getAttribute('href')==='/themes/${theme}.css')return resolve(true);l.onload=()=>resolve(true);l.onerror=()=>reject(Error('theme missing'));l.href='/themes/${theme}.css';})`);
            for (const [w, h] of [[1440, 1100], [390, 844]]) {
                await b.viewport(w, h);
                await b.evaluate("document.querySelector('textarea[aria-label=\"证书链 PEM 内容\"]').closest('.cw-row').scrollIntoView({block:'start'})");
                assert.equal(await b.evaluate("document.documentElement.scrollWidth<=innerWidth && [...document.querySelectorAll('[data-page=network] textarea')].every(e=>{const r=e.getBoundingClientRect();return r.width>100&&r.left>=0&&r.right<=innerWidth;})"), true);
                shots.push(await b.shot(`certificates-${theme}-${w}`));
            }
        }
    });
    console.log(JSON.stringify({runtime: process.env.LUCI_DEVICE_APPEND === '1' ? 'upstream plus observed append replay' : 'pinned upstream', checks, failures, errors: b.errors, shots}, null, 2));
    assert.deepEqual(failures, []);
    assert.deepEqual(b.errors, []);
} finally { await b.close(); await luci.close(); await api.close(); }
