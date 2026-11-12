// Continuous user workflows in real Chrome. All phones, API and media are synthetic.
import assert from 'node:assert/strict';
import {start} from './lib/ui-browser.mjs';
import {startFixture} from './lib/ui-fixture.mjs';
import {confirmation} from './lib/confirmation-contract.mjs';

const results=[];
async function scenario(name,run){
  if(process.env.COWPLAY_FLOW_CASES&&!process.env.COWPLAY_FLOW_CASES.split(',').includes(name))return;
  const service=await startFixture();let b;
  try{b=await start();}catch(error){await service.close();throw error;}
  const {origin,fixture}=service;
  const frame=()=>b.evaluate('new Promise(resolve=>requestAnimationFrame(()=>requestAnimationFrame(()=>resolve(true))))');
  async function click(selector){
    const group=await b.evaluate(`(()=>{const e=document.querySelector(${JSON.stringify(selector)}),g=e?.closest('.control-group');return g&&!g.open&&e!==g.firstElementChild?g.id:null})()`);
    if(group)await click('#'+group+' > summary');
    const at=await b.evaluate(`(()=>{const e=document.querySelector(${JSON.stringify(selector)});if(!e)throw Error('Missing control');e.scrollIntoView({block:'nearest'});const r=e.getBoundingClientRect();return {x:r.left+r.width/2,y:r.top+r.height/2}})()`);
    await b.call('Input.dispatchMouseEvent',{type:'mousePressed',...at,button:'left',clickCount:1});
    await b.call('Input.dispatchMouseEvent',{type:'mouseReleased',...at,button:'left',clickCount:1});
    await frame();
    if(['#sheet-stop','#r-re','#sheet-picture-apply'].includes(selector)||selector.startsWith('#sheet-phones '))await confirmation(b);
  }
  async function select(selector,value,wait=true){
    if(selector.startsWith('#sheet-picture-')&&!selector.includes('-max-')&&!await b.evaluate(`!!document.querySelector(${JSON.stringify(selector)})`)){
      await click('#sheet-quality .picture-presets button:nth-child(4)');
    }
    const previous=await b.evaluate(`document.querySelector(${JSON.stringify(selector)}).value`);
    await b.evaluate(`(()=>{const e=document.querySelector(${JSON.stringify(selector)});e.focus();e.value=${JSON.stringify(value)};e.dispatchEvent(new Event('change',{bubbles:true}))})()`);
    await frame();
    if(selector.includes('-max-')&&previous!==value)await confirmation(b);
    if(selector.startsWith('#sheet-picture-')&&!selector.includes('-max-')&&!await b.evaluate("document.querySelector('#sheet-picture-apply').disabled"))await click('#sheet-picture-apply');
    if(wait)await b.wait(()=>b.evaluate(`!document.querySelector(${JSON.stringify(selector)}).disabled`),'setting save completes');
  }
  async function controls(){await click('#r-sheet');await b.wait(()=>b.evaluate("!document.querySelector('#sheet').hidden"),'control centre');if(!await b.evaluate("document.querySelector('#sheet-picture-group').open"))await click('#sheet-picture-group > summary');if(!await b.evaluate("!!document.querySelector('#sheet-picture-resolution')"))await click('#sheet-quality .picture-presets button:nth-child(4)');await b.evaluate("document.querySelector('#sheet-scroll').scrollTop=0");await frame();}
  async function heldClick(selector){
    const group=await b.evaluate(`(()=>{const g=document.querySelector(${JSON.stringify(selector)})?.closest('.control-group');return g&&!g.open?g.id:null})()`);
    if(group)await click('#'+group+' > summary');
    const at=await b.evaluate(`(()=>{const e=document.querySelector(${JSON.stringify(selector)});e.scrollIntoView({block:'nearest'});const r=e.getBoundingClientRect();return {x:r.left+r.width/2,y:r.top+r.height/2,polls:qaPolls}})()`);
    await b.call('Input.dispatchMouseEvent',{type:'mousePressed',x:at.x,y:at.y,button:'left',clickCount:1});
    await b.wait(()=>b.evaluate(`qaPolls>${at.polls}`),'a complete state refresh while the control is pressed');await frame();
    await b.call('Input.dispatchMouseEvent',{type:'mouseReleased',x:at.x,y:at.y,button:'left',clickCount:1});await frame();
    if(selector.startsWith('#sheet-phones '))await confirmation(b);
  }
  const commonVisible=()=>b.evaluate("(()=>{const box=document.querySelector('#sheet-scroll').getBoundingClientRect();return [...document.querySelectorAll('#sheet-toggles button')].every(e=>{const r=e.getBoundingClientRect();return r.top>=box.top&&r.bottom<=box.bottom})})()");
  try{
    await b.call('Page.addScriptToEvaluateOnNewDocument',{source:`(()=>{
      window.qaPolls=0;window.qaFrames=0;window.qaSaves=0;const request=window.fetch.bind(window);
      window.fetch=async(...args)=>{const r=await request(...args);if(args[0]==='/api/state'){await r.clone().json();window.qaPolls++}if(['/api/prefs','/api/policy'].includes(args[0])){await r.clone().json();window.qaSaves++}return r};
      const pixels=new OffscreenCanvas(1208,720),ctx=pixels.getContext('2d');ctx.fillStyle='#17333a';ctx.fillRect(0,0,1208,720);ctx.fillStyle='#fff';ctx.font='26px sans-serif';ctx.fillText('完整流程模拟 · 非真实手机画面',460,130);
      window.VideoDecoder=class{state='unconfigured';decodeQueueSize=0;constructor(init){this.init=init}configure(){this.state='configured'}decode(c){setTimeout(()=>{if(this.state==='configured'){this.init.output(new VideoFrame(pixels,{timestamp:c.timestamp}));window.qaFrames++}},0)}close(){this.state='closed'}static async isConfigSupported(c){return {supported:true,config:c}}};
    })()`});
    await fixture({mode:'session',auth:true,phase:'closed',open:false,clear:true});
    await b.viewport(1280,720);await b.goto(origin+'/car');
    await b.wait(()=>b.evaluate("document.body.dataset.view==='session'"),'first painted frame');
    await run({b,origin,fixture,frame,click,select,controls,heldClick,commonVisible});
    assert.deepEqual(b.errors,[]);
    results.push({name,status:'passed'});
  }catch(error){results.push({name,status:'failed',error:String(error),state:await b.evaluate("({view:document.body.dataset.view,side:document.body.dataset.side,tokenVisible:document.querySelector('#token-layer')?.offsetParent!==null,polls:window.qaPolls})"),shot:await b.shot('flow-'+name),browserErrors:b.errors});}
  finally{await b.close();await service.close();}
}

await scenario('carplay-navigation',async({b,fixture,click,controls})=>{
  assert.deepEqual(await b.evaluate("[...document.querySelectorAll('.rail .cell')].map(e=>e.id)"),['r-sheet','r-phones','r-full','r-re','r-siri','r-back','r-home']);
  assert.equal(await b.evaluate("document.querySelector('.status-chip')===null"),true,'unlabelled status pill is removed');
  await click('#r-sheet');await click('#r-siri');
  await b.wait(async()=>(await fixture({})).controlMessages.some(m=>m.type==='siri'),'rail Siri sends the existing command');
  assert.equal(await b.evaluate("document.querySelector('#sheet').hidden"),true,'Siri reveals the phone picture');
  await click('#r-back');
  await b.wait(async()=>(await fixture({})).controlMessages.some(m=>m.type==='knob'&&m.knob.back&&!m.knob.select),'rail Back sends the existing back command');
  await click('#r-home');
  await b.wait(async()=>(await fixture({})).homeRequests===1,'desktop shortcut sends a real home command');
  assert.equal(await b.evaluate('location.hash'),'','home is a phone command, not browser navigation');
  assert.equal((await fixture({})).cmds.length,0,'home does not reconnect or disconnect the phone');
  await click('#r-phones');
  await b.wait(()=>b.evaluate("document.body.dataset.view==='settings'&&!!document.querySelector('#s-pane .add-row')"),'direct iPhone management entry');
  await click('#s-back');await controls();
  assert.equal(await b.evaluate("new URL(document.querySelector('#sheet-admin a').href).pathname"),'/luci');
  assert.equal(await b.evaluate("document.querySelector('#sheet-admin a').target"),'_blank','management preserves the projection tab');
  await click('#sheet-done');
  await fixture({wsText:{type:'host-ui'}});
  await b.wait(()=>b.evaluate("document.body.dataset.view==='settings'&&!!document.querySelector('#settings-picture-controls')"),'CarPlay CowPlay opens picture settings regardless of the previous tab');
  assert.equal(await b.evaluate("document.querySelector('#s-title').textContent"),'CowPlay 设置');
  await click('#s-back');await b.wait(()=>b.evaluate("document.body.dataset.view==='session'"),'return from CowPlay settings keeps the picture');
  for(const [w,h] of [[1280,720],[800,480],[390,844]]){
    await b.viewport(w,h);
    await b.wait(()=>b.evaluate("(()=>{const e=document.querySelector('#r-home'),r=e.getBoundingClientRect();return r.top>innerHeight/2&&r.bottom<=innerHeight&&getComputedStyle(e.querySelector('.cell-label')).display!=='none'})()"),'labelled home stays at the bottom');
    assert.equal(await b.evaluate("(()=>{const ids=['r-siri','r-back','r-home'];return ids.every((id,i)=>{const e=document.getElementById(id),r=e.getBoundingClientRect();return r.height>=44&&r.top>=0&&r.bottom<=innerHeight&&document.elementFromPoint(r.left+r.width/2,r.top+r.height/2)?.closest('button')===e&&(!i||r.top>=document.getElementById(ids[i-1]).getBoundingClientRect().bottom)})})()"),true,'Siri, Back and Home stay visible in that order with touch targets');
    await b.shot(`navigation-${w}`);
  }
  await fixture({sessionReset:true});
  await b.wait(()=>b.evaluate("['r-siri','r-back','r-home'].every(id=>document.getElementById(id).disabled)"),'phone shortcuts are unavailable while the picture is not ready');
});

await scenario('dismiss-controls-from-picture',async({b,fixture,click,frame})=>{
  const touchReports=async()=>(await fixture({})).controlMessages.filter(m=>m.type==='touch'&&m.contacts.length>0);
  for(const side of ['left','right'])for(const [w,h] of [[1280,720],[800,480],[390,844]]){
    await fixture({prefsPatch:{rail_side:side}});
    await b.wait(()=>b.evaluate(`document.body.dataset.side==='${side}'`),'control rail side');
    await b.viewport(w,h);await frame();
    for(const touch of [false,true]){
      await b.call('Emulation.setTouchEmulationEnabled',{enabled:touch,maxTouchPoints:2});
      await click('#r-sheet');
      await click('#sheet-playback-group > summary');
      assert.equal(await b.evaluate("document.querySelector('#sheet').hidden"),false,'interacting inside the menu keeps it open');
      const at=await b.evaluate(`(()=>{const r=document.querySelector('.video').getBoundingClientRect();return{x:r.left+r.width*${side==='left'?.85:.15},y:r.top+r.height*.5}})()`);
      assert.equal(await b.evaluate(`!!document.elementFromPoint(${at.x},${at.y})?.closest('.video')`),true,'dismissal point reaches the picture outside the menu');
      const before=(await touchReports()).length;
      const tap=async(point=at)=>{
        if(touch){
          await b.call('Input.dispatchTouchEvent',{type:'touchStart',touchPoints:[{id:1,...point}]});
          await b.call('Input.dispatchTouchEvent',{type:'touchEnd',touchPoints:[]});
        }else{
          await b.call('Input.dispatchMouseEvent',{type:'mousePressed',...point,button:'left',clickCount:1});
          await b.call('Input.dispatchMouseEvent',{type:'mouseReleased',...point,button:'left',clickCount:1});
        }
        await frame();
      };
      await tap();
      assert.deepEqual(await b.evaluate("[document.querySelector('#sheet').hidden,document.querySelector('#r-sheet').getAttribute('aria-pressed'),document.querySelector('#screen').inert]"),[true,'false',false],'clicking outside closes the menu and restores picture input');
      assert.equal((await touchReports()).length,before,'the dismissal tap must not operate CarPlay');
      if(side==='left'&&!touch)await b.shot(`controls-dismissed-${w}`);
      // Compact layouts expand the picture again when the menu closes.
      const nextAt=await b.evaluate("(()=>{const r=document.querySelector('#screen').getBoundingClientRect();return{x:r.left+r.width*.85,y:r.top+r.height*.5}})()");
      await tap(nextAt);
      await b.wait(async()=>{const m=(await touchReports()).slice(before);return m.some(x=>x.contacts.some(c=>c.down))&&m.some(x=>x.contacts.some(c=>!c.down))},'the next picture tap reaches CarPlay');
      await click('#r-sheet');await click('#r-sheet');
      assert.equal(await b.evaluate("document.querySelector('#sheet').hidden"),true,'the control button still toggles the menu closed');
    }
  }
  assert.deepEqual(await b.evaluate("[location.pathname,document.title,!!document.querySelector('vite-error-overlay')]"),['/car','CowPlay · 车机',false]);
  assert.deepEqual(b.warnings,[]);
});

await scenario('grouped-controls',async({b,fixture,click,frame})=>{
  assert.deepEqual(await b.evaluate("[location.pathname,document.title,!!document.querySelector('vite-error-overlay')]"),['/car','CowPlay · 车机',false]);
  await click('#r-sheet');
  assert.deepEqual(await b.evaluate("[...document.querySelectorAll('.control-group > summary > span')].map(e=>e.firstChild.textContent)"),['播放与操控','声音与通话','画面与显示','iPhone','诊断与管理']);
  assert.equal(await b.evaluate("document.querySelectorAll('.control-group[open]').length"),0,'all functional groups start collapsed');
  assert.equal(await b.evaluate("!!document.querySelector('#sheet-input [data-action=siri],#sheet-input [data-action=back]')"),false,'rail shortcuts are not repeated in the panel');
  for(const [w,h] of [[1280,720],[800,480],[390,844]]){
    await b.viewport(w,h);await frame();
    assert.equal(await b.evaluate("(()=>{const box=document.querySelector('#sheet-scroll').getBoundingClientRect();return [...document.querySelectorAll('.control-group > summary')].every(e=>{const r=e.getBoundingClientRect();return r.height>=44&&r.top>=box.top&&r.bottom<=box.bottom&&document.elementFromPoint(r.left+r.width/2,r.top+r.height/2)?.closest('summary')===e})})()"),true,'collapsed categories are visible and reachable without scrolling');
    assert.equal(await b.evaluate("['sheet-done','sheet-all','sheet-stop'].every(id=>{const e=document.getElementById(id),r=e.getBoundingClientRect();return document.elementFromPoint(r.left+r.width/2,r.top+r.height/2)?.closest('#'+id)===e})"),true,'close, settings and disconnect remain reachable');
    assert.equal(await b.evaluate("document.documentElement.scrollWidth<=innerWidth"),true,'no page overflow');
    await b.shot(`grouped-controls-${w}`);
  }
  await b.viewport(1280,720);await click('#sheet-playback-group > summary');
  await click('#sheet-input [data-action=play]');
  await click('#sheet-input [data-action=select]');
  await b.wait(async()=>{const m=(await fixture({})).controlMessages;return m.some(x=>x.type==='media'&&x.media===3)&&m.some(x=>x.type==='knob'&&x.knob.select&&!x.knob.back)},'grouped playback and selection retain commands');
  const polls=await b.evaluate('qaPolls');await b.wait(()=>b.evaluate(`qaPolls>${polls}`),'poll after group expansion');
  assert.equal(await b.evaluate("document.querySelector('#sheet-playback-group').open"),true,'state polling preserves expansion');
  await click('#sheet-playback-group > summary');
  assert.equal(await b.evaluate("document.querySelector('#sheet-playback-group').open"),false,'summary collapses its own controls');
  await b.evaluate("document.querySelector('#sheet-playback-group > summary').focus()");
  for(const [key,code,vk,open] of [['Enter','Enter',13,true],[' ','Space',32,false]]){
    await b.call('Input.dispatchKeyEvent',{type:'keyDown',key,code,windowsVirtualKeyCode:vk,text:key==='Enter'?'\r':' '});
    await b.call('Input.dispatchKeyEvent',{type:'keyUp',key,code,windowsVirtualKeyCode:vk});
    await frame();
    assert.equal(await b.evaluate("document.querySelector('#sheet-playback-group').open"),open,'native Enter/Space toggles groups without sending phone input');
  }
  await click('#sheet-sound-group > summary');await click('#sheet-audio .audio-mute');
  assert.equal(await b.evaluate("document.querySelector('#sheet-audio .audio-mute').getAttribute('aria-pressed')"),'true','grouped volume controls still update');
  assert.equal(await b.evaluate("document.querySelector('#sheet-audio .audio-mute').textContent"),'取消静音');
  await click('#sheet-audio .audio-mute');
  assert.equal(await b.evaluate("document.querySelector('#sheet-audio .audio-mute').getAttribute('aria-pressed')"),'false','mute can be toggled again while focused');
  await click('#sheet-picture-group > summary');
  await click('#sheet-toggles button');
  await b.wait(async()=>(await fixture({})).prefs.night,'grouped display switch saves');
  await click('#sheet-diagnostics-group > summary');await click('#sheet-debug button');
  await b.wait(()=>b.evaluate("!document.querySelector('#stats').hidden"),'grouped diagnostics switch opens debug overlay');
  assert.equal(await b.evaluate("document.querySelector('#sheet-diagnostics-group').open"),true,'saving keeps the current group open');
  assert.deepEqual((await fixture({})).cmds,[],'expansion and local controls do not reconnect or disconnect the phone');
  assert.deepEqual(b.warnings,[],'no browser console warnings');
  await b.shot('grouped-controls-expanded');
});

await scenario('reopen-controls-at-common-actions',async({b,frame,click,controls,commonVisible})=>{
  await controls();await b.evaluate("document.querySelector('#sheet-scroll').scrollTop=9999");await frame();
  assert.ok(await b.evaluate("document.querySelector('#sheet-scroll').scrollTop>0"));
  await click('#sheet-done');await controls();
  assert.equal(await commonVisible(),true,'reopening controls must start at the common actions after scrolling advanced settings');
  await b.shot('flow-reopened-controls');
});

await scenario('switch-held-across-state-poll',async({b,fixture,frame,controls})=>{
  await controls();
  const at=await b.evaluate("(()=>{const e=document.querySelector('#sheet-toggles button');window.qaPressed=e;const r=e.getBoundingClientRect();return{x:r.left+r.width/2,y:r.top+r.height/2,polls:qaPolls}})()");
  await b.call('Input.dispatchMouseEvent',{type:'mousePressed',x:at.x,y:at.y,button:'left',clickCount:1});
  await b.wait(()=>b.evaluate(`qaPolls>${at.polls}`),'one state refresh while holding the switch');await frame();
  await b.call('Input.dispatchMouseEvent',{type:'mouseReleased',x:at.x,y:at.y,button:'left',clickCount:1});
  await frame();
  await b.wait(async()=>(await fixture({})).prefs.night===true,'release still toggles night appearance');
});

await scenario('saving-does-not-silently-drop-switch',async({b,fixture,frame,click,select,controls})=>{
  await controls();await fixture({prefsHold:true});await select('#sheet-picture-resolution','2560x1440',false);
  await b.wait(async()=>(await fixture({})).pendingPrefs===1,'save request held by the service');
  await b.wait(()=>b.evaluate("document.querySelector('#sheet-picture-resolution').disabled"),'save pending');
  const disabled=await b.evaluate("document.querySelector('#sheet-toggles button').disabled");
  await b.shot('flow-controls-saving');
  if(!disabled)await click('#sheet-toggles button');
  await fixture({prefsHold:false,releasePrefs:'ok'});await frame();
  await b.wait(()=>b.evaluate("!document.querySelector('#sheet-picture-resolution').disabled"),'save released');
  assert.ok(disabled||(await fixture({})).prefs.night,'a switch must be unavailable or its accepted click must be saved');
});

await scenario('late-save-error-does-not-change-new-client',async({b,fixture,frame,click,select,controls})=>{
  await controls();await fixture({prefsHold:true});
  await b.evaluate("location.hash='settings/display'");
  await b.wait(()=>b.evaluate("document.body.dataset.view==='settings'&&document.querySelectorAll('#s-pane .side-card').length===2"),'display settings');
  await b.evaluate("[...document.querySelectorAll('.side-card')].find(e=>e.textContent.includes('右侧')).click()");await frame();
  await b.wait(async()=>(await fixture({})).pendingPrefs===1,'old client save request held by the service');
  await fixture({auth:false,clientId:'new-synthetic-screen',prefsPatch:{rail_side:'right'}});
  await b.wait(()=>b.evaluate("!document.querySelector('#token-layer').hidden"),'authorization revoked');
  await b.evaluate("for(const k of '123456')[...document.querySelectorAll('#keys button')].find(e=>e.textContent===k).click()");
  await b.wait(()=>b.evaluate("document.body.dataset.view==='settings'&&document.querySelector('.side-card[aria-checked=true]')?.textContent.includes('右侧')"),'new client returns to the requested settings with its own side');
  await click('#s-back');await b.wait(()=>b.evaluate("document.body.dataset.view==='session'"),'return to the painted session');
  await controls();await fixture({prefsHold:false,releasePrefs:'fail'});await frame();
  await b.wait(()=>b.evaluate("!document.querySelector('#sheet-picture-resolution').disabled"),'old save settled');
  assert.equal(await b.evaluate('document.body.dataset.side'),'right','an old client error must not restore its side on the new client');
  await b.shot('flow-reauthorized-client-controls');
});

for(const resource of ['prefs','policy'])for(const outcome of ['stale-ok','fail']){
  await scenario(`readmission-pending-state-${resource}-${outcome}`,async({b,fixture,frame,select,controls})=>{
    await controls();await fixture({prefsHold:resource==='prefs',policyHold:resource==='policy'});
    if(resource==='policy')await b.evaluate("document.querySelector('#sheet .picture-advanced').open=true");
    await select(resource==='prefs'?'#sheet-picture-fps':'#sheet-picture-max-fps','30',false);
    await b.wait(async()=>{const f=await fixture({});return (resource==='prefs'?f.pendingPrefs:f.pendingPolicies)===1},'old save is pending');
    await fixture({auth:false,clientId:'readmitted-screen',prefsPatch:{rail_side:'right',quality:'auto',fps:60},policyPatch:{max_fps:60}});
    await b.wait(()=>b.evaluate("document.querySelector('#token-layer').offsetParent!==null"),'authorization revoked before saving returns');
    await fixture({stateHold:true});
    await b.evaluate("for(const k of '123456')[...document.querySelectorAll('#keys button')].find(e=>e.textContent===k).click()");
    await b.wait(async()=>{const f=await fixture({});return f.authorized&&f.pendingStates===1},'admission completed but its new state is still pending');
    await fixture(resource==='prefs'?{prefsHold:false,releasePrefs:outcome}:{policyHold:false,releasePolicy:outcome});
    await b.wait(()=>b.evaluate('qaSaves===1'),'old response reached the browser');await frame();
    assert.equal(await b.evaluate("/未保存|已保存|请求超时/.test(document.querySelector('.toast')?.textContent??'')"),false,'the previous authorization must not report its save outcome during readmission');
    await fixture({stateHold:false,releaseState:true});
    await b.wait(()=>b.evaluate("document.body.dataset.view==='session'&&document.body.dataset.side==='right'"),'new client uses its own authoritative state');
    await controls();
    assert.equal(await b.evaluate("document.querySelector('#sheet-picture-fps').value"),'60');
    assert.equal(await b.evaluate("document.querySelector('#sheet-picture-max-fps').value"),'60');
  });
}

await scenario('same-client-readmission-discards-old-optimistic-prefs',async({b,fixture,select,controls})=>{
  await controls();await fixture({prefsHold:true});await select('#sheet-picture-fps','30',false);
  await b.wait(async()=>(await fixture({})).pendingPrefs===1,'old optimistic setting is pending');
  await fixture({auth:false,prefsPatch:{rail_side:'right',quality:'auto',fps:60}});
  await b.wait(()=>b.evaluate("document.querySelector('#token-layer').offsetParent!==null"),'authorization revoked');
  await fixture({stateHold:true});
  await b.evaluate("for(const k of '123456')[...document.querySelectorAll('#keys button')].find(e=>e.textContent===k).click()");
  await b.wait(async()=>(await fixture({})).pendingStates===1,'new authorization state pending for the same client ID');
  await fixture({stateHold:false,releaseState:true});
  await b.wait(()=>b.evaluate("document.body.dataset.view==='session'"),'readmitted session paints');
  assert.equal(await b.evaluate('document.body.dataset.side'),'right','new authorization must accept server preferences before the old save ends');
  await controls();assert.equal(await b.evaluate("document.querySelector('#sheet-picture-fps').value"),'60');
  await fixture({prefsHold:false,releasePrefs:'fail'});
});

for(const resource of ['prefs','policy']){
  await scenario(`old-${resource}-response-cannot-unlock-new-save`,async({b,fixture,frame,select,controls})=>{
    await controls();await fixture({prefsHold:resource==='prefs',policyHold:resource==='policy'});
    if(resource==='policy')await b.evaluate("document.querySelector('#sheet .picture-advanced').open=true");
    await select(resource==='prefs'?'#sheet-picture-fps':'#sheet-picture-max-fps','30',false);
    await b.wait(async()=>{const f=await fixture({});return (resource==='prefs'?f.pendingPrefs:f.pendingPolicies)===1},'old save is pending');
    await fixture({auth:false,clientId:'new-saving-screen',prefsPatch:{rail_side:'right',quality:'auto',fps:60}});
    await b.wait(()=>b.evaluate("document.querySelector('#token-layer').offsetParent!==null"),'authorization revoked');
    await b.evaluate("for(const k of '123456')[...document.querySelectorAll('#keys button')].find(e=>e.textContent===k).click()");
    await b.wait(()=>b.evaluate("document.body.dataset.view==='session'&&document.body.dataset.side==='right'"),'new client paints');
    await controls();
    assert.equal(await b.evaluate("document.querySelector('#sheet-picture-fps').disabled"),false,'a new authorization can save without waiting for the old request');
    await fixture({prefsHold:true});await select('#sheet-picture-fps','30',false);
    await b.wait(async()=>(await fixture({})).pendingPrefs===(resource==='prefs'?2:1),'new save remains pending');
    await fixture(resource==='prefs'?{releasePrefs:'stale-ok',releaseCount:1}:{releasePolicy:'stale-ok'});
    await b.wait(()=>b.evaluate('qaSaves===1'),'old successful response reaches the browser');await frame();
    assert.equal(await b.evaluate("document.querySelector('#sheet-picture-fps').disabled"),true,'old finally must not unlock the new save');
    await fixture({prefsHold:false,releasePrefs:'ok'});
    await b.wait(()=>b.evaluate("!document.querySelector('#sheet-picture-fps').disabled"),'new save completes and unlocks its own controls');
    assert.equal((await fixture({})).prefs.fps,30);
  });
}

for(const failNewState of [false,true]){
  await scenario(`readmission-bypasses-old-state-${failNewState?'after-error':'direct'}`,async({b,fixture,frame})=>{
    await fixture({stateHold:true});
    await b.wait(async()=>(await fixture({})).pendingStates===1,'an old authorization state request is pending');
    await fixture({auth:false,clientId:'new-state-screen',prefsPatch:{rail_side:'right'},wsText:{type:'error',code:'AUTH_REVOKED'}});
    await b.wait(()=>b.evaluate("document.querySelector('#token-layer').offsetParent!==null"),'WebSocket revokes authorization while the old state request is in flight');
    const before=await fixture(failNewState?{stateHold:false,stateFail:true}:{});
    await b.evaluate("for(const k of '123456')[...document.querySelectorAll('#keys button')].find(e=>e.textContent===k).click()");
    if(failNewState){
      await b.wait(async()=>(await fixture({})).stateReads>before.stateReads,'new authorization attempts its own state request despite the old pending one');
      assert.equal(await b.evaluate('document.body.dataset.view'),'connect','failed state loading must not reopen the old session');
      await fixture({stateFail:false});
    }else{
      await b.wait(async()=>(await fixture({})).pendingStates===2,'new authorization has its own state request before the old one returns');
      await fixture({releaseState:'newest'});
    }
    await b.wait(()=>b.evaluate("document.body.dataset.view==='session'&&document.body.dataset.side==='right'"),'new authorization completes using its own state');
    const polls=await b.evaluate('qaPolls');await fixture({stateHold:false,releaseState:true});
    await b.wait(()=>b.evaluate(`qaPolls>${polls}`),'the old state response finally arrives');await frame();
    assert.equal(await b.evaluate('document.body.dataset.side'),'right','old state response must not replace the new authorization state');
  });
}

await scenario('failed-disconnect-can-be-retried',async({b,fixture,click,controls})=>{
  await controls();await fixture({cmdFail:true});await click('#sheet-stop');
  await b.wait(()=>b.evaluate("document.querySelector('.toast')?.textContent.includes('合成操作失败')"),'disconnect failure feedback');
  assert.equal(await b.evaluate('document.body.dataset.view'),'session');
  await fixture({cmdFail:false});await click('#sheet-stop');
  await b.wait(()=>b.evaluate("document.body.dataset.scene==='stopped'"),'retry disconnect stops CarPlay');
  assert.equal((await fixture({})).stopped,true);
});

for(const mode of ['session','takeover']){
  await scenario(`readmission-reconciles-lifted-stop-${mode}`,async({b,fixture,click,controls})=>{
    await controls();await click('#sheet-stop');
    await b.wait(()=>b.evaluate("document.body.dataset.scene==='stopped'"),'this screen stopped CarPlay');
    await fixture({auth:false});
    await b.wait(()=>b.evaluate("document.querySelector('#token-layer').offsetParent!==null"),'stopped screen loses authorization');
    await fixture({mode,stopped:false,clientId:'readmitted-after-stop'});
    await b.evaluate("for(const k of '123456')[...document.querySelectorAll('#keys button')].find(e=>e.textContent===k).click()");
    if(mode==='session')await b.wait(()=>b.evaluate("document.body.dataset.view==='session'"),'new authoritative state lifts the old local stop');
    else{
      await b.wait(()=>b.evaluate("document.body.dataset.scene==='takeover'"),'the other screen stays in control after authorization returns');
      assert.equal(await b.evaluate("[...document.querySelectorAll('#c-body button')].some(e=>e.textContent==='在这块屏幕显示')"),true,'resume must not start an automatic takeover countdown');
      const before=await fixture({});
      await b.wait(async()=>(await fixture({})).stateReads>=before.stateReads+2,'two later state polls exceed the takeover countdown');
      assert.equal((await fixture({})).wsAttempts,before.wsAttempts,'no automatic takeover after reauthorization');
    }
  });
}

await scenario('disconnect-while-saving-stays-stopped',async({b,fixture,frame,click,select,controls})=>{
  await controls();await fixture({prefsHold:true});await select('#sheet-picture-fps','30',false);
  await b.wait(async()=>(await fixture({})).pendingPrefs===1,'save pending before disconnect');
  await click('#sheet-stop');
  await b.wait(()=>b.evaluate("document.body.dataset.scene==='stopped'"),'stop during save');
  const before=await fixture({});await fixture({prefsHold:false,releasePrefs:'ok'});await frame();
  await b.wait(async()=>(await fixture({})).stateReads>=before.stateReads+2,'stop survives later state refreshes');
  const after=await fixture({});assert.equal(after.stopped,true);assert.equal(after.wsAttempts,before.wsAttempts);
  assert.equal(await b.evaluate('document.body.dataset.scene'),'stopped');
});

await scenario('saved-phones-switch-a-b-a',async({b,fixture,click,controls})=>{
  const state=await (await fetch((await b.evaluate('location.origin'))+'/api/state')).json();const ids=state.phones.map(p=>p.id);
  await controls();
  for(const name of ['另一部 iPhone','测试 iPhone']){
    const index=await b.evaluate(`[...document.querySelectorAll('#sheet-phones button')].findIndex(e=>e.textContent.includes(${JSON.stringify(name)}))`);
    await click(`#sheet-phones button:nth-child(${index+1})`);
    await b.wait(()=>b.evaluate(`[...document.querySelectorAll('#sheet-phones button')].some(e=>e.getAttribute('aria-checked')==='true'&&e.textContent.includes(${JSON.stringify(name)}))`),'selected phone is current');
  }
  const after=await (await fetch(await b.evaluate("location.origin+'/api/state'"))).json();assert.deepEqual(after.phones.map(p=>p.id),ids);
  const cmds=(await fixture({})).cmds;assert.deepEqual(cmds.filter(c=>c.cmd==='switchPhone').map(c=>c.id),[ids[1],ids[0]]);assert.equal(cmds.some(c=>c.cmd==='forgetPhone'),false);
});

await scenario('independent-settings-survive-reload',async({b,origin,fixture,select,controls})=>{
  await controls();await select('#sheet-picture-resolution','1920x1080');await select('#sheet-picture-fps','30');
  await fixture({prefsPatch:{rail_side:'right'}});await b.goto(origin+'/car?persisted');
  await b.wait(()=>b.evaluate("document.body.dataset.view==='session'&&document.body.dataset.side==='right'"),'saved settings on reload');
  await controls();assert.equal(await b.evaluate("document.querySelector('#sheet-picture-resolution').value"),'1920x1080');
  assert.equal(await b.evaluate("document.querySelector('#sheet-picture-fps').value"),'30');
});

await scenario('picture-preset-held-across-poll',async({b,fixture,heldClick,controls})=>{
  await controls();await b.wait(()=>b.evaluate('qaPolls>=2'),'viewport report settled');
  await heldClick('#sheet-quality .picture-presets button:nth-child(3)');
  assert.equal((await fixture({})).prefs.quality,'auto','preset editing remains a draft');
  await b.evaluate("document.querySelector('#sheet-picture-apply').click()");await confirmation(b);
  await b.wait(async()=>(await fixture({})).prefs.quality==='sharp','applying saves the held preset choice');
});

await scenario('phone-switch-held-across-poll',async({fixture,heldClick,controls})=>{
  await controls();await heldClick('#sheet-phones button:nth-child(2)');
  await fixture({});
  assert.equal((await fixture({})).cmds.some(c=>c.cmd==='switchPhone'&&c.id.endsWith('_77')),true,'phone selection must survive a state refresh during the press');
});

await scenario('settings-choice-held-across-poll',async({b,fixture,heldClick})=>{
  await b.evaluate("location.hash='settings/display'");
  await b.wait(()=>b.evaluate("document.querySelectorAll('#s-pane .side-card').length===2"),'display choices');
  await heldClick('#s-pane .side-card:nth-child(2)');
  await b.wait(async()=>(await fixture({})).prefs.rail_side==='right','settings choice release changes the side');
});

await scenario('settings-navigation-held-across-poll',async({b,heldClick})=>{
  await b.evaluate("location.hash='settings/pic'");
  await b.wait(()=>b.evaluate("!!document.querySelector('#settings-picture-controls')"),'picture settings');
  await heldClick('#s-tabs .s-tab:nth-child(3)');
  await b.wait(()=>b.evaluate("location.hash==='#settings/display'&&document.querySelectorAll('#s-pane .side-card').length===2"),'held navigation still opens the requested settings');
});

await scenario('device-tab-held-across-poll',async({b,heldClick})=>{
  await b.evaluate("location.hash='settings/devices'");
  await b.wait(()=>b.evaluate("document.querySelector('#s-pane [aria-label=设备]')"),'device tabs');
  await heldClick('#s-pane [aria-label=设备] button:nth-child(3)');
  await b.wait(()=>b.evaluate("document.querySelector('#s-pane [aria-label=设备] button:nth-child(3)').getAttribute('aria-selected')==='true'"),'held device tab still selects iPhone');
});

await scenario('microphone-denial-retry-and-stop',async({b,fixture,click,controls})=>{
  await controls();
  await b.evaluate("navigator.mediaDevices.getUserMedia=async()=>{throw new DOMException('Synthetic denial','NotAllowedError')}");
  await fixture({wsText:{type:'mic',state:'start',rate:16000,channels:1,codec:'lpcm',samplesPerPacket:320,bitrate:null,epoch:1}});
  await b.wait(()=>b.evaluate("document.querySelector('.toast')?.textContent.includes('麦克风未授权或不可用')"),'microphone denial is visible');
  await b.shot('flow-microphone-denied');
  assert.equal(await b.evaluate('document.body.dataset.view'),'session');
  await b.evaluate("(async()=>{window.qaMicContext=new AudioContext();await qaMicContext.resume();window.qaMicStream=qaMicContext.createMediaStreamDestination().stream;navigator.mediaDevices.getUserMedia=async()=>qaMicStream})()");
  await fixture({wsText:{type:'mic',state:'start',rate:16000,channels:1,codec:'lpcm',samplesPerPacket:320,bitrate:null,epoch:1}});
  await b.wait(async()=>(await fixture({})).micPackets>0,'synthetic silent microphone samples reach the receiver');
  await fixture({wsText:{type:'mic',state:'stop'}});
  await b.wait(()=>b.evaluate("qaMicStream.getTracks().every(t=>t.readyState==='ended')"),'stop releases the synthetic capture track');
  await b.evaluate('qaMicContext.close()');
});

await scenario('resume-frozen-page-without-false-stall',async({b,fixture})=>{
  const before=await b.evaluate('qaFrames');
  await b.call('Page.setWebLifecycleState',{state:'frozen'});
  await new Promise(resolve=>setTimeout(resolve,2500));
  await b.call('Page.setWebLifecycleState',{state:'active'});
  await b.wait(()=>b.evaluate(`qaFrames>${before}+2&&document.querySelector('#cover').hidden`),'drawing resumes after page suspension');
  assert.equal(await b.evaluate("document.querySelector('#stall').hidden"),true);
  assert.equal((await fixture({})).cmds.some(c=>c.cmd==='restartVideo'||c.cmd==='reconnect'),false,'healthy resumed drawing must not trigger an iPhone reconnect');
});

console.log(JSON.stringify({synthetic:true,results},null,2));
if(results.some(r=>r.status==='failed'))process.exitCode=1;
