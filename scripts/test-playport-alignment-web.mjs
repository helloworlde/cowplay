// User-visible PlayPort alignment regressions. API, phones and video remain synthetic.
import assert from 'node:assert/strict';
import {start} from './lib/ui-browser.mjs';
import {startFixture} from './lib/ui-fixture.mjs';
import {confirmation} from './lib/confirmation-contract.mjs';
const service=await startFixture(), b=await start(), results=[];
const frame=()=>b.evaluate('new Promise(r=>requestAnimationFrame(()=>requestAnimationFrame(r)))');
async function change(id,value){
 if(!await b.evaluate(`!!document.getElementById(${JSON.stringify(id)})`)){await b.evaluate(`[...document.querySelectorAll('.picture-presets button')].find(e=>e.textContent==='自定义').click()`);await frame();}
 await b.evaluate(`(()=>{const e=document.getElementById(${JSON.stringify(id)});e.value=${JSON.stringify(value)};e.dispatchEvent(new Event('change',{bubbles:true}));})()`);await frame();}
async function check(name,run){try{await run();results.push({name,passed:true});}catch(e){results.push({name,passed:false,error:String(e),shot:await b.shot('alignment-'+results.length)});}}
try{
 await b.call('Page.addScriptToEvaluateOnNewDocument',{source:`(()=>{const canvas=new OffscreenCanvas(1208,720);canvas.getContext('2d').fillRect(0,0,1208,720);window.VideoDecoder=class {state='unconfigured';decodeQueueSize=0;constructor(init){this.init=init}configure(){this.state='configured'}decode(c){if(this.state==='configured')this.init.output(new VideoFrame(canvas,{timestamp:c.timestamp}))}close(){this.state='closed'}static async isConfigSupported(config){return {supported:config.codec==='hvc1.1.6.L93.B0',config}}};})()`});
 await service.fixture({mode:'connecting',auth:true});
 await b.viewport(390,844);await b.goto(service.origin+'/car#settings/display');
 await b.wait(()=>b.evaluate("!!document.querySelector('#s-pane h1')"),'settings');
 await check('390px settings keep a readable content column',async()=>{
  assert.ok(await b.evaluate("document.querySelector('#s-pane').getBoundingClientRect().width>=340"));
  assert.ok(await b.evaluate("document.querySelector('#s-pane').scrollWidth<=document.querySelector('#s-pane').clientWidth"));
  for(const tab of ['pic','audio','display','phones','devices','diag','about']){
   await b.evaluate(`location.hash='settings/${tab}'`);await frame();
   assert.ok(await b.evaluate("(()=>{const e=document.querySelector('.s-tab[aria-current=page]').getBoundingClientRect(),box=document.querySelector('#s-tabs').getBoundingClientRect();return e.left>=box.left-1&&e.right<=box.right+1})()"),'selected narrow navigation group remains visible: '+tab);
   assert.ok(await b.evaluate("document.querySelector('#s-pane').scrollWidth<=document.querySelector('#s-pane').clientWidth"),'narrow content does not overflow: '+tab);
  }
 });
 await b.viewport(1280,720);await b.goto(service.origin+'/car#settings/pic');
 await b.wait(()=>b.evaluate("!!document.querySelector('#settings-picture-controls')"),'picture');
 await check('picture settings show only relevant fields and keep advanced controls folded',async()=>{
  assert.equal(await b.evaluate("document.querySelector('#settings-picture-resolution')===null"),true,'automatic preset needs no manual parameters');
  assert.equal(await b.evaluate("document.querySelector('.picture-advanced').open"),false);
  for(const [width,height] of [[1280,720],[390,844]]){
   await b.viewport(width,height);await frame();
   assert.ok(await b.evaluate("document.querySelector('#settings-picture-reset').getBoundingClientRect().bottom<=innerHeight"),'recovery fits in the initial viewport');
   assert.equal(await b.evaluate("document.querySelector('#settings-picture-controls').scrollWidth<=document.querySelector('#settings-picture-controls').clientWidth"),true);
  }
  await b.evaluate("[...document.querySelectorAll('.picture-presets button')].find(e=>e.textContent==='自定义').click()");await frame();
  assert.equal(await b.evaluate("document.querySelectorAll('#settings-picture-controls>.picture-fields>.picture-field').length"),3);
  await b.evaluate("document.querySelector('.picture-advanced>summary').click()");await frame();
  await service.fixture({policyPatch:{max_fps:30}});
  await b.wait(()=>b.evaluate("document.querySelector('#settings-picture-max-fps').value==='30'"),'fresh policy poll');
  assert.equal(await b.evaluate("document.querySelector('.picture-advanced').open"),true,'advanced disclosure survives the poll');
  await service.fixture({policyPatch:{max_fps:60}});
  await b.wait(()=>b.evaluate("document.querySelector('#settings-picture-max-fps').value==='60'"),'restore policy');
  await b.evaluate("document.querySelector('.picture-advanced>summary').click()");await frame();await b.viewport(1280,720);
 });
 await check('display changes stay local until a single apply',async()=>{
  const writes=(await service.fixture({})).prefsWrites;
  await b.evaluate("for(const [id,value] of [['settings-picture-fps','30'],['settings-picture-codec','hevc']]){const e=document.getElementById(id);e.value=value;e.dispatchEvent(new Event('change',{bubbles:true}));}");await frame();
  assert.equal((await service.fixture({})).prefs.fps,60,'editing must not reconnect the phone');
  await b.wait(()=>b.evaluate("!document.querySelector('#settings-picture-apply').disabled"),'apply available');
  await b.evaluate("const apply=document.querySelector('#settings-picture-apply');apply.click();apply.click();");await confirmation(b);
  await b.wait(async()=>{const f=await service.fixture({});return f.prefs.fps===30&&f.prefs.codec==='hevc'},'one applied preference set');
  assert.equal((await service.fixture({})).prefsWrites,writes+1);
  await b.wait(()=>b.evaluate("document.querySelector('#settings-picture-error').textContent==='已保存画面设置。'"),'accurate save confirmation');
  assert.equal(await b.evaluate("document.querySelector('#settings-picture-error').classList.contains('picture-error')"),false,'a successful save is not an error or a stale waiting state');
 });
 await b.viewport(390,844);await b.goto(service.origin+'/phone#settings');
 await b.wait(()=>b.evaluate("document.documentElement.dataset.ready==='true'"),'phone');
 await check('phone can edit independent video fields and restore compatible quality',async()=>{
  await b.evaluate("[...document.querySelectorAll('button')].find(e=>e.textContent.startsWith('画质')).click()");await frame();
  assert.ok(await b.evaluate("!!document.querySelector('#phone-picture-codec')&&!!document.querySelector('#phone-picture-reset')"));
  await b.evaluate("document.querySelector('#phone-picture-reset').click()");await confirmation(b);
  await b.wait(async()=>{const p=(await service.fixture({})).prefs;return p.codec==='h264'&&p.fps===30&&p.resolution==='720p'},'phone compatibility recovery');
  await b.wait(()=>b.evaluate("!document.querySelector('#phone-picture-fps').disabled"),'recovery response releases editor');
 });
 await check('failed apply keeps the draft and retry saves it once',async()=>{
  await change('phone-picture-fps','60');await service.fixture({prefsFail:true});
  await b.evaluate("document.querySelector('#phone-picture-apply').click()");await confirmation(b);
  await b.wait(()=>b.evaluate("document.querySelector('#phone-picture-error')?.textContent.includes('未保存')"),'save failure shown');
  assert.equal((await service.fixture({})).prefs.fps,30);assert.equal(await b.evaluate("document.querySelector('#phone-picture-fps').value"),'60');
  await service.fixture({prefsFail:false});await b.evaluate("document.querySelector('#phone-picture-apply').click()");await confirmation(b);
  await b.wait(async()=>(await service.fixture({})).prefs.fps===60,'retry applies draft');
  await b.wait(()=>b.evaluate("!document.querySelector('#phone-picture-resolution').disabled"),'retry response releases editor');
 });
 await check('invalid custom dimensions stay editable and cannot silently fall back',async()=>{
  await change('phone-picture-resolution','custom');await change('phone-picture-width','-2');
  assert.equal(await b.evaluate("document.querySelector('#phone-picture-width').value"),'-2');
  assert.equal(await b.evaluate("document.querySelector('#phone-picture-apply').disabled"),true);
  await change('phone-picture-width','320');await change('phone-picture-height','2160');
  assert.equal(await b.evaluate("document.querySelector('#phone-picture-apply').disabled"),true,'aspect cannot fit protocol minima within 1080p');
  await change('phone-picture-width','720');await change('phone-picture-height','1280');
  assert.equal(await b.evaluate("document.querySelector('#phone-picture-apply').disabled"),false);
  assert.ok(await b.evaluate("document.querySelector('#phone-picture-controls').textContent.includes('保持比例缩小')"));
 });
 await check('invalid custom dimensions recover through every non-custom preset',async()=>{
  for(const [width,preset,quality,resolution,fps] of [['-2','自动','auto','native',60],['721','流畅','smooth','native',30],['4000','清晰','sharp','1.5x',60]]){
   await change('phone-picture-resolution','custom');await change('phone-picture-width',width);
   assert.equal(await b.evaluate("document.querySelector('#phone-picture-apply').disabled"),true);
   await b.evaluate(`[...document.querySelectorAll('#phone-picture-controls .picture-presets button')].find(e=>e.textContent===${JSON.stringify(preset)}).click()`);await frame();
   await b.evaluate("document.querySelector('#phone-picture-apply').click()");await confirmation(b);
   await b.wait(async()=>{const p=(await service.fixture({})).prefs;return p.quality===quality&&p.resolution===resolution&&p.fps===fps&&p.codec==='h264'},'valid preset replaces hidden invalid fields');
   await b.wait(()=>b.evaluate("!document.querySelector('#phone-picture-controls .picture-presets button').disabled"),'preset response completes');
  }
 });
 await check('phone rail labels remain separate from CarPlay driver side',async()=>{
  await b.evaluate("[...document.querySelectorAll('#overlay button')].find(e=>e.textContent==='取消').click()");await frame();
  await b.evaluate("[...document.querySelectorAll('#content button')].find(e=>e.textContent.startsWith('控制栏位置')).click()");await frame();
  assert.equal(await b.evaluate("document.querySelector('#overlay').textContent.includes('右舵车')"),false);
  assert.equal(await b.evaluate("document.querySelector('#overlay').textContent.includes('控制栏靠右')"),true);
 });
 await check('touch release and keyboard isolation survive opening controls',async()=>{
  await service.fixture({mode:'session'});await b.viewport(1280,720);await b.goto(service.origin+'/car?input');
  await b.wait(()=>b.evaluate("document.body.dataset.view==='session'"),'synthetic frame');
  // Native capture requires a real pointer; use CDP for the actual held contact.
  await b.call('Input.dispatchMouseEvent',{type:'mousePressed',x:600,y:300,button:'left',clickCount:1});await frame();
  const before=(await service.fixture({})).controlMessages.length;
  await b.evaluate("document.querySelector('#r-sheet').click()");await frame();
  await b.call('Input.dispatchMouseEvent',{type:'mouseReleased',x:600,y:300,button:'left',clickCount:1});await frame();
  const messages=(await service.fixture({})).controlMessages.slice(before);
  assert.ok(messages.some(m=>m.type==='touch'&&m.contacts.length===0),'opening controls releases the held contact');
  const count=(await service.fixture({})).controlMessages.length;
  await b.call('Input.dispatchKeyEvent',{type:'keyDown',key:'s',code:'KeyS',windowsVirtualKeyCode:83});await frame();
  assert.equal((await service.fixture({})).controlMessages.length,count,'panel keyboard does not control CarPlay');
  await b.evaluate("document.querySelector('#r-siri').click()");await frame();
  assert.equal((await service.fixture({})).controlMessages.at(-1).type,'siri');
  await b.evaluate("document.querySelector('#sheet-done').click();document.querySelector('#screen').focus()");await frame();
  await b.call('Input.dispatchKeyEvent',{type:'keyDown',key:'ArrowRight',code:'ArrowRight',windowsVirtualKeyCode:39});await frame();
  assert.equal((await service.fixture({})).controlMessages.at(-1).knob.x,127);
 });
 await check('browser mixer changes volume without reconnecting and persists on reload',async()=>{
  await b.goto(service.origin+'/car#settings/audio');await b.wait(()=>b.evaluate("!!document.querySelector('.audio-mixer')"),'mixer');
  const writes=(await service.fixture({})).prefsWrites;
  await b.evaluate("(()=>{const e=document.querySelector('.audio-slider input');e.value='35';e.dispatchEvent(new Event('input',{bubbles:true}));e.dispatchEvent(new Event('change',{bubbles:true}));})()");await frame();
  assert.equal((await service.fixture({})).prefsWrites,writes);
  await b.goto(service.origin+'/car?audio-reload#settings/audio');await b.wait(()=>b.evaluate("!!document.querySelector('.audio-mixer')"),'saved mixer');
  assert.equal(await b.evaluate("document.querySelector('.audio-slider input').value"),'35');
 });
 await check('phone never presents a requested fps cap as current drawing and clears failed samples',async()=>{
  await service.fixture({statsTimestamp:Date.now()});
  await b.viewport(390,844);await b.goto(service.origin+'/phone');await b.wait(()=>b.evaluate("document.documentElement.dataset.ready==='true'"),'phone status');
  await b.wait(()=>b.evaluate("document.querySelector('.live').textContent.includes('59.9 fps')"),'fresh drawing sample');
  await service.fixture({statsFail:true});
  await b.wait(()=>b.evaluate("document.querySelector('.live').textContent.includes('帧率待采样')"),'failed samples cleared');
  await service.fixture({statsFail:false});
 });
 await check('idle phone with failed stats stays usable and newer server samples are accepted',async()=>{
  await service.fixture({mode:'connecting',statsFail:true});await b.goto(service.origin+'/phone?idle-failure');
  await b.wait(()=>b.evaluate("document.documentElement.dataset.ready==='true'"),'idle status without stats');
  assert.ok(await b.evaluate("document.querySelector('#content').textContent.includes('正在连接')"));
  await service.fixture({mode:'session',statsFail:false,statsTimestamp:Date.now()+500});
  await b.wait(()=>b.evaluate("document.querySelector('.live').textContent.includes('59.9 fps')"),'newer server sample accepted');
 });
 console.log(JSON.stringify({results,browserErrors:b.errors},null,2));
 assert.ok(results.every(x=>x.passed),'alignment scenarios must pass');assert.deepEqual(b.errors,[]);
}finally{await b.close();await service.close();}
