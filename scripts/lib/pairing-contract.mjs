// Built pages and a synthetic API; all phone identities and media are placeholders.
import assert from 'node:assert/strict';
import {start} from './ui-browser.mjs';
import {startFixture} from './ui-fixture.mjs';
import {confirmation} from './confirmation-contract.mjs';

export async function runPairingContract(kind) {
  const service = await startFixture(), {origin, fixture} = service;
  const b = await start(), checks = [], shots = [], car = kind === 'car', prefix = car ? 'car-' : 'phone-';
  const value = expression => b.evaluate(expression);
  const code = digits => b.wait(() => value(`document.getElementById('${prefix}passkey')?.textContent.replace(/\\s/g,'')===${JSON.stringify(digits)}&&!document.getElementById('${prefix}accept').disabled`), 'visible code '+digits);
  const click = async label => {
    assert.ok(await value(`(()=>{const e=[...document.querySelectorAll('button,a')].find(e=>e.offsetParent!==null&&!e.disabled&&(e.getAttribute('aria-label')??e.textContent.trim())===${JSON.stringify(label)});if(!e)return false;e.click();return true})()`), label);
  };
  const action = type => value(`document.getElementById('${prefix}${type}').click()`);
  const commands = async type => (await fixture({})).cmds.filter(x => x.cmd === type);
  const load = async route => { await b.goto(origin+route); await b.wait(() => value(car ? "!!document.querySelector('#c-title')?.textContent" : "document.documentElement.dataset.ready==='true'"), 'page'); };
  try {
    await fixture({mode:'connecting',auth:true,generation:7,phase:'Confirm',request:1,code:'000042',open:true,clear:true});
    await b.viewport(car ? 800 : 390, car ? 480 : 844);
    await load(car ? '/car' : '/pair'); await code('000042');
    assert.equal(await value(`getComputedStyle(document.getElementById('${prefix}passkey')).color`),'rgb(255, 255, 255)');
    await value(`document.getElementById('${prefix}accept').focus()`); await new Promise(r=>setTimeout(r,2200));
    assert.equal(await value('document.activeElement.id'),prefix+'accept');
    assert.equal((await commands('connect')).length,0);
    // Dispatch the double click before a successful reply removes the confirmation controls.
    await value(`(()=>{const accept=document.getElementById('${prefix}accept');accept.click();accept.click()})()`);
    await b.wait(async () => (await commands('confirmPair')).length===1, 'one confirmation');
    assert.deepEqual((await commands('confirmPair'))[0],{cmd:'confirmPair',generation:7,request:1,accept:true});
    checks.push('leading-zero code, displayed request snapshot, duplicate guard, no old-phone invitation');
    if (car) await value("location.hash='settings'"); else await load('/phone#settings');
    await fixture({generation:8,phase:'Confirm',request:2,code:'123456',open:true,confirmFail:true}); await code('123456');
    if (car) assert.equal(await value("document.querySelector('#car-pairing-dialog').open"),true);
    await action('accept');
    await b.wait(() => value(`document.querySelector('.pairing-confirmation')?.textContent.includes('合成确认失败')&&!document.getElementById('${prefix}reject').disabled`), 'failed confirmation retry');
    await action('reject'); await b.wait(async () => (await commands('confirmPair')).length===3, 'reject request');
    assert.deepEqual((await commands('confirmPair'))[2],{cmd:'confirmPair',generation:8,request:2,accept:false});
    checks.push('settings confirmation, failure remains retryable, explicit rejection');
    await fixture({generation:9,phase:'Confirm',request:3,code:'654321',open:true,confirmHold:true}); await code('654321');
    await action('accept'); await b.wait(async () => (await commands('confirmPair')).length===4, 'held confirmation');
    await fixture({generation:10,request:4,code:'009999'});
    await b.wait(() => value(`document.getElementById('${prefix}passkey')?.textContent.replace(/\\s/g,'')==='009999'`), 'replacement request');
    await fixture({releaseConfirm:'过时请求错误'}); await code('009999');
    assert.equal(await value("document.querySelector('.pairing-confirmation').textContent.includes('过时请求错误')"),false);
    checks.push('late old response cannot contaminate the replacement request');
    await fixture({stateFail:true});
    await b.wait(() => value(`document.getElementById('${prefix}accept').disabled`), 'read failure blocks confirmation');
    await fixture({stateFail:false}); await code('009999');
    assert.equal(await value(`document.querySelectorAll('#${prefix}accept').length`),1);
    checks.push('state-read failure disables decisions and recovers one confirmation instance');
    await fixture({confirmHold:true}); await action('accept');
    await b.wait(async () => (await commands('confirmPair')).length===5, 'held request before access change');
    await fixture({auth:false});
    await b.wait(() => value(car ? "!document.querySelector('#token-layer').hidden" : "!!document.querySelector('.auth')"), 'authorization revoked');
    await fixture({clientId:'new-'+kind,releaseConfirm:'旧授权请求错误',generation:11,request:5,code:'006688'});
    if (car) {
      for (const n of '123456') await value(`[...document.querySelectorAll('#keys button')].find(e=>e.textContent==='${n}').click()`);
    } else await value("document.querySelector('.auth input').value='123456';document.querySelector('.auth').requestSubmit()");
    await code('006688');
    assert.equal(await value("document.querySelector('.pairing-confirmation').textContent.includes('旧授权请求错误')"),false);
    checks.push('authorization loss and readmission isolate the old response');
    shots.push(await b.shot(kind+'-manual-confirmation'));
    if (car) {
      for (const phase of ['Waiting','Confirm','Pairing']) {
        await fixture({phase,request:phase==='Confirm'?6:null,code:phase==='Confirm'?'001122':null,open:true,clear:true});
        await load('/car?phase='+phase);
        await b.wait(async () => (await fixture({})).wsAttempts>0, 'viewer channel during '+phase);
        await new Promise(r=>setTimeout(r,2200));
        assert.equal((await commands('connect')).length,0,phase+' must preserve pairing');
      }
      checks.push('viewer startup preserves Waiting, Confirm and Pairing');
      // The F50 invites a just-paired phone itself; the car names it and never targets an older phone.
      const NEW='/org/bluez/hci0/dev_00_11_22_33_44_99';
      await fixture({phase:'Paired',request:null,code:null,open:false,device:NEW,newPhone:NEW,link:{bluetooth:'active',wifi:'idle'},clear:true});
      await load('/car?paired'); await b.wait(() => value("document.querySelector('#c-title').textContent==='新 iPhone'"), 'car names the new phone being invited');
      await new Promise(r=>setTimeout(r,2200));
      assert.ok((await commands('connect')).every(c => !c.id || c.id===NEW), 'an older phone is never called explicitly');
      assert.equal(await value("[...document.querySelectorAll('#c-body button')].some(e=>e.textContent==='连接 CarPlay')"), false);
      await fixture({phase:'Cancelled',link:{bluetooth:'fail',wifi:'idle',error:{step:'bluetooth',title:'配对成功，但自动连接未完成',hint:'点“重新连接”。',ts_ms:Date.now()}},clear:true});
      await b.wait(() => value("[...document.querySelectorAll('#c-body button')].some(e=>e.textContent==='重新连接')&&document.querySelector('#c-title').textContent==='新 iPhone'"), 'failed invitation keeps the new phone');
      await click('重新连接'); await b.wait(async () => (await commands('connect')).some(c => c.id===NEW), 'retry targets the new phone');
      await fixture({newPhone:null,device:null,link:{}});
      checks.push('after bonding the car names and retries only the new phone');
      await fixture({phase:'Confirm',generation:12,request:7,code:'000042',open:true});
      await b.viewport(390,844); await load('/car?portrait'); await code('000042');
      assert.equal(await value("document.querySelector('#car-pairing-dialog').open"),true);
      assert.ok(await value("document.documentElement.scrollWidth<=innerWidth"));
      shots.push(await b.shot('car-manual-confirmation-390'));
      checks.push('portrait confirmation is visible and has no document overflow');
    } else {
      await fixture({phase:'closed',request:null,code:null,open:false,device:null,clear:true}); await load('/phone#iphones');
      await click('＋ 添加 iPhone');await confirmation(b); await b.wait(() => value("!!document.querySelector('.bottom-sheet')"), 'add sheet');
      let generation = (await fixture({})).pairing.generation;
      const X='/org/bluez/hci0/dev_00_11_22_33_44_99';
      const devices=[{id:X,name:'iPhone X',iphone:true,phone:true,rssi:-47},{id:X+'0',name:'很长的蓝牙名称 <script>不会执行</script>',iphone:false,phone:false,rssi:-60}];
      await fixture({nearby:[...devices,...Array.from({length:10},(_,i)=>({...devices[1],id:X+i,name:'合成设备 '+i}))]});
      await b.wait(() => value("document.querySelectorAll('.nearby-phone').length===12"), 'long discovered list');
      const sheetScroll=await value("(()=>{const s=document.querySelector('.bottom-sheet');s.scrollTop=s.scrollHeight;return s.scrollTop})()");
      assert.ok(sheetScroll>100);await new Promise(r=>setTimeout(r,2200));assert.ok(await value(`document.querySelector('.bottom-sheet').scrollTop>=${sheetScroll}-1`));
      await load('/pair?long-list');await b.wait(()=>value("document.querySelectorAll('.nearby-phone').length===12"),'long standalone list');
      const pairScroll=await value("(()=>{const s=document.querySelector('.pair-scroll');s.scrollTop=s.scrollHeight;return s.scrollTop})()");
      assert.ok(pairScroll>100);await new Promise(r=>setTimeout(r,2200));assert.ok(await value(`document.querySelector('.pair-scroll').scrollTop>=${pairScroll}-1`));
      checks.push('long add-sheet and standalone lists retain scroll position through polling');
      await load('/phone#iphones');await click('＋ 添加 iPhone');await confirmation(b);generation=(await fixture({})).pairing.generation;
      await fixture({nearby:devices});
      await b.wait(() => value("document.querySelectorAll('.nearby-phone').length===2"), 'discovered phones');
      assert.equal(await value("document.querySelectorAll('.nearby-phones script').length"),0);
      assert.ok(await value("document.querySelector('.nearby-phones').textContent.includes('cowplay')&&document.documentElement.scrollWidth<=innerWidth"));
      shots.push(await b.shot('phone-nearby-390'));
      await value("document.querySelector('.nearby-phone').click();document.querySelector('.nearby-phone').click()");
      await b.wait(async () => (await commands('pairDevice')).length===1, 'outgoing pairing');
      assert.deepEqual((await commands('pairDevice'))[0],{cmd:'pairDevice',device:X,generation});
      assert.equal(await value("document.querySelector('.sheet-head button').disabled"),false);
      checks.push('nearby names render as text; exact target and generation; cancel stays available');
      await fixture({phase:'Confirm',request:8,code:'000042'}); await code('000042'); await action('accept');
      await b.wait(async () => (await commands('confirmPair')).some(c=>c.generation===generation&&c.request===8), 'outgoing confirmation');
      checks.push('outgoing Pair leaves manual confirmation usable');
      await fixture({phase:'Cancelled',open:false,code:null,request:null,pairingError:'iPhone 拒绝配对，请重试'});
      await b.wait(() => value("document.querySelector('.bottom-sheet').textContent.includes('iPhone 拒绝配对')"), 'asynchronous failure');
      await new Promise(r=>setTimeout(r,2200)); assert.ok(await value("document.querySelector('.bottom-sheet').textContent.includes('iPhone 拒绝配对')"));
      assert.equal(await value("document.querySelector('.nearby-phones').textContent.includes('正在配对…')"),false);
      await click('重新开始配对');await confirmation(b); await b.wait(() => value("!document.querySelector('.bottom-sheet').textContent.includes('iPhone 拒绝配对')&&document.querySelectorAll('.nearby-phone').length===0"), 'rescan clears stale results');
      checks.push('async failure survives polling; rescan clears obsolete results and errors');
      await click('取消');await b.wait(()=>value("!document.querySelector('.bottom-sheet')"),'cancel ACK closes the old pairing panel');
      await fixture({phase:'Paired',open:false,request:null,code:null,device:X,newPhone:X,stopped:false,link:{},clear:true}); await load('/pair');
      await b.wait(() => value("document.querySelector('h1').textContent==='正在连接 CarPlay'"), 'bond waits for the automatic CarPlay invitation');
      assert.equal(await value("document.querySelector('.pair-done')===null"),true);
      await new Promise(r=>setTimeout(r,2200)); assert.equal((await commands('connect')).length,0,'the page does not race the F50 invitation');
      await fixture({phase:'Cancelled',link:{bluetooth:'fail',wifi:'idle',carplay:'idle',error:{step:'bluetooth',title:'配对成功，但自动连接未完成',hint:'点“重新连接”。',ts_ms:Date.now()}}});
      await b.wait(() => value("document.querySelector('h1').textContent==='连接未完成'"), 'failed invitation');
      await click('重新连接');assert.equal(await value("!!document.querySelector('.action-confirmation[open]')"),false,'retry after a failed idle invitation does not claim another live session will be interrupted'); await b.wait(async () => (await commands('connect')).length===1, 'retry new saved phone');
      assert.equal((await commands('connect'))[0].id,X);
      await fixture({newPhone:null,device:null,link:{}});
      checks.push('bond auto-connects; a failed invitation retries the explicit new target');
      await fixture({mode:'session',phase:'closed',clear:true}); await load('/phone#iphones');
      for (const name of ['另一部 iPhone','测试 iPhone']) {
        await value(`(()=>{const row=[...document.querySelectorAll('.phone-row')].find(e=>e.textContent.includes(${JSON.stringify(name)}));row.querySelector('.tint').click()})()`);await confirmation(b);
        await b.wait(() => value(`[...document.querySelectorAll('.phone-row')].some(e=>e.textContent.includes(${JSON.stringify(name)})&&e.querySelector('.current'))`), 'selected '+name);
      }
      assert.deepEqual((await commands('switchPhone')).map(x=>x.id.slice(-2)),['77','66']);
      checks.push('saved-phone A to B to A uses explicit targets and retains records');
    }
    assert.deepEqual(b.errors,[]); console.log(JSON.stringify({kind,checks,shots,errors:b.errors},null,2));
  } catch (e) {
    console.log(JSON.stringify({kind,error:String(e),text:await value('document.body.innerText'),state:await fixture({}),shot:await b.shot(kind+'-pairing-failure')})); throw e;
  } finally { await b.close(); await service.close(); }
}
