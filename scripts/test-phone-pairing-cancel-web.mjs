// Pairing cancellation acknowledgements in isolated Chrome; all phones and RPC replies are synthetic.
import assert from 'node:assert/strict';
import {start} from './lib/ui-browser.mjs';
import {startFixture} from './lib/ui-fixture.mjs';

const results=[];
const NEW='/org/bluez/hci0/dev_00_11_22_33_44_99';
async function scenario(name,run){
 const f=await startFixture();let b;
 try{
  b=await start();await b.viewport(390,844);
  await b.call('Page.addScriptToEvaluateOnNewDocument',{source:`(()=>{
   const request=window.fetch.bind(window);window.qaCancelMode='normal';window.qaCancelCalls=0;window.qaCancelReplies=[];
   window.fetch=async(...args)=>{
    if(args[0]!=='/api/cmd'||JSON.parse(args[1]?.body||'{}').cmd!=='closePairWindow')return request(...args);
    window.qaCancelCalls++;
    if(window.qaCancelMode==='missing-ack')return new Response(JSON.stringify({accepted:true}),{status:200});
    const response=await request(...args);
    return window.qaCancelMode==='hold'?new Promise(resolve=>window.qaCancelReplies.push(()=>resolve(response))):response;
   };
  })()`});
  await f.fixture({mode:'connecting',auth:true,phase:'closed',open:false,link:{bluetooth:'idle',wifi:'idle',carplay:'idle'},clear:true});
  const value=expression=>b.evaluate(expression);
  const frame=()=>value('new Promise(r=>requestAnimationFrame(()=>requestAnimationFrame(r)))');
  const load=async route=>{await b.goto(f.origin+route);await b.wait(()=>value("document.documentElement.dataset.ready==='true'&&!!document.querySelector('#content .group')"),'phone loaded');};
  const click=async expression=>{assert.ok(await value(`(()=>{const e=${expression};if(!e||e.disabled)return false;e.click();return true})()`),'enabled action');await frame();};
  const head=()=>value("document.querySelector('.bottom-sheet .sheet-head button')?.textContent");
  const cancelCount=async()=>(await f.fixture({})).cmds.filter(c=>c.cmd==='closePairWindow').length;
  const open=async()=>{
   await load('/phone#iphones');await click("[...document.querySelectorAll('#content button')].find(e=>e.textContent==='＋ 添加 iPhone')");
   await b.wait(()=>value("!!document.querySelector('.bottom-sheet')"),'add sheet');
  };
  const cancel=()=>click("document.querySelector('.bottom-sheet .sheet-head button')");
  await run({b,f,value,frame,load,click,head,cancelCount,open,cancel});
  assert.deepEqual(b.errors,[]);results.push({name,status:'passed'});
 }catch(error){results.push({name,status:'failed',error:String(error),errors:b?.errors,shot:b?await b.shot('pairing-cancel-'+name):null});}
 finally{if(b)await b.close();await f.close();}
}

for(const [phase,patch,text] of [
 ['waiting',{},'正在等待新 iPhone'],
 ['code',{phase:'Confirm',request:1,code:'123456'},'核对两端的配对码'],
 ['finishing',{phase:'Pairing',request:null,code:null},'正在保存配对'],
])await scenario(phase+'-failure-retry',async({b,f,value,open,cancel,cancelCount})=>{
 await open();await f.fixture(patch);await b.wait(()=>value(`document.querySelector('.bottom-sheet').textContent.includes(${JSON.stringify(text)})`),'active '+phase);
 await f.fixture({cmdFail:true});await cancel();
 await b.wait(()=>value("document.querySelector('.bottom-sheet [role=alert]')?.textContent.includes('合成操作失败')&&!document.querySelector('.bottom-sheet .sheet-head button').disabled"),'failure remains in sheet');
 assert.equal((await f.fixture({})).pairing.open,true);assert.equal(await cancelCount(),1);
 await f.fixture({cmdFail:false});await cancel();await b.wait(()=>value("!document.querySelector('.bottom-sheet')"),'acknowledged cancel closes sheet');
 assert.equal((await f.fixture({})).pairing.open,false);assert.equal(await cancelCount(),2);
});

await scenario('pending-cancel-keeps-sheet-and-blocks-duplicates',async({b,value,frame,open,cancel,cancelCount})=>{
 await open();await value("window.qaCancelMode='hold'");await cancel();
 await b.wait(()=>value('window.qaCancelReplies.length===1'),'held acknowledgement');
 assert.equal(await value("document.querySelector('.bottom-sheet .sheet-head button').textContent"),'正在取消…');
 assert.equal(await value("document.querySelector('.bottom-sheet .sheet-head button').disabled&&document.querySelector('#overlay .dim').disabled"),true);
 await value("document.querySelector('.bottom-sheet .sheet-head button').click();document.querySelector('#overlay .dim').click()");
 await b.call('Input.dispatchKeyEvent',{type:'keyDown',key:'Escape',code:'Escape',windowsVirtualKeyCode:27});await frame();
 assert.equal(await value("!!document.querySelector('.bottom-sheet')"),true);assert.equal(await cancelCount(),1);assert.equal(await value('window.qaCancelCalls'),1);
 await value('window.qaCancelReplies.shift()()');await b.wait(()=>value("!document.querySelector('.bottom-sheet')"),'ack closes sheet');
});

await scenario('missing-explicit-ack-keeps-sheet',async({b,f,value,open,cancel})=>{
 await open();await value("window.qaCancelMode='missing-ack'");await cancel();
 await b.wait(()=>value("document.querySelector('.bottom-sheet [role=alert]')?.textContent.includes('尚未得到确认')"),'missing ack is visible');
 assert.equal((await f.fixture({})).pairing.open,true);
 await value("window.qaCancelMode='normal'");await cancel();await b.wait(()=>value("!document.querySelector('.bottom-sheet')"),'normal ack retries');
});

await scenario('late-ack-keeps-new-settings-panel',async({b,value,frame,open,cancel,click})=>{
 await open();await value("window.qaCancelMode='hold'");await cancel();await b.wait(()=>value('window.qaCancelReplies.length===1'),'held ack');
 await value("location.hash='settings'");await b.wait(()=>value("document.querySelector('#content h1')?.textContent==='设置'"),'new page');
 await click("[...document.querySelectorAll('#content button')].find(e=>e.textContent.startsWith('画质'))");
 await b.wait(()=>value("!!document.querySelector('#phone-picture-controls')"),'new picture panel');
 await value('window.qaCancelReplies.shift()()');await frame();assert.equal(await value("!!document.querySelector('#phone-picture-controls')"),true);
});

await scenario('late-ack-keeps-new-pairing-generation',async({b,f,value,frame,open,cancel})=>{
 await open();await value("window.qaCancelMode='hold'");await cancel();await b.wait(()=>value('window.qaCancelReplies.length===1'),'held old ack');
 const generation=(await f.fixture({})).pairing.generation;
 await f.fixture({generation:generation+1,phase:'Waiting',open:true,request:null,code:null});
 await b.wait(()=>value("document.querySelector('.bottom-sheet .sheet-head button')?.textContent==='取消'&&!document.querySelector('.bottom-sheet .sheet-head button').disabled"),'new generation can be cancelled');
 await value('window.qaCancelReplies.shift()()');await frame();
 assert.equal(await value("!!document.querySelector('.bottom-sheet')"),true);assert.equal((await f.fixture({})).pairing.open,true);
});

for(const live of [false,true])await scenario((live?'connected':'connecting')+'-close-keeps-saved-connection',async({b,f,value,open,cancel,cancelCount})=>{
 await open();await f.fixture({mode:live?'session':'connecting',phase:live?'Cancelled':'Paired',open:false,device:NEW,newPhone:NEW,request:null,code:null});
 await b.wait(()=>value("document.querySelector('.bottom-sheet .sheet-head button')?.textContent==='关闭'"),'saved connection uses close');
 assert.equal(await value("document.querySelector('.bottom-sheet').textContent.includes('关闭面板不会')"),true);
 const n=await cancelCount();await cancel();assert.equal(await value("!!document.querySelector('.bottom-sheet')"),false);assert.equal(await cancelCount(),n);
});

await scenario('standalone-cancel-failure-remains-retryable',async({b,f,value,load,click})=>{
 await f.fixture({phase:'Waiting',open:true,cmdFail:true});await load('/pair');
 await click("document.querySelector('.pair-bottom button.secondary')");
 await b.wait(()=>value("document.querySelector('#content [role=alert]')?.textContent.includes('合成操作失败')&&!document.querySelector('.pair-bottom button.secondary').disabled"),'standalone cancel failure');
 assert.equal((await f.fixture({})).pairing.open,true);await f.fixture({cmdFail:false});await click("document.querySelector('.pair-bottom button.secondary')");
 await b.wait(async()=>!(await f.fixture({})).pairing.open,'standalone retry acknowledged');
});

console.log(JSON.stringify({results},null,2));if(results.some(r=>r.status==='failed'))process.exitCode=1;
