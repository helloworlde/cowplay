// Synthetic URL-keyed stale document/cache model; no Tesla hardware or audio.
import assert from 'node:assert/strict';
import {start} from './lib/ui-browser.mjs';
import {startFixture} from './lib/ui-fixture.mjs';

const checks=[],shots=[];
const strict=process.env.COWPLAY_TESLA_BROWSER==='1';
for(const limited of [false,true]){
 const service=await startFixture({staleCar:true}),b=await start();
 try{
  if(strict)assert.equal((await b.call('Browser.getVersion')).product,'Chrome/136.0.7103.92');
  await b.viewport(strict?1254:1280,strict?784:720,strict?1.53:1);
  if(limited){
   await b.call('Emulation.setUserAgentOverride',{userAgent:strict?'Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/136.0.7103.92 Safari/537.36':'Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/109.0.0.0 Safari/537.36 Tesla/Synthetic',platform:'Linux x86_64'});
   await b.call('Page.addScriptToEvaluateOnNewDocument',{source:`
    delete window.VideoDecoder;delete window.AudioDecoder;
    delete navigator.wakeLock;Object.defineProperty(navigator,'wakeLock',{value:undefined});
    delete Array.prototype.toSorted;delete Array.prototype.findLast;delete Array.fromAsync;delete Promise.withResolvers;
   `});
  }
  await b.call('Page.addScriptToEvaluateOnNewDocument',{source:`window.pageInstance=Date.now()+Math.random();window.helloSeen=0;const Native=window.WebSocket;window.WebSocket=class extends Native{constructor(...args){super(...args);this.addEventListener('message',event=>{if(typeof event.data==='string'&&JSON.parse(event.data).type==='hello')helloSeen++})}};`});
  await service.fixture({auth:true,mode:'connecting',stopped:false});
  await b.goto(service.origin+'/car');
  if(strict)assert.deepEqual(await b.evaluate('({width:innerWidth,height:innerHeight,dpr:devicePixelRatio})'),{width:1254,height:784,dpr:Math.fround(1.53)});
  await b.wait(()=>b.evaluate('helloSeen>0'),'cached document does not prevent viewer initialization');
  assert.match(await b.evaluate('document.title'),/CowPlay/);
  assert.equal(await b.evaluate("!!document.querySelector('vite-error-overlay')"),false);
  const previous=await b.evaluate('pageInstance');await b.call('Page.reload');
  await b.wait(async()=>await b.evaluate('pageInstance')!==previous&&await b.evaluate('helloSeen>0'),'ordinary reload still connects');
  for(const accessOpen of [true,false]){
   await service.fixture({accessOpen,auth:true});const before=await b.evaluate('pageInstance');await b.call('Page.reload');
   await b.wait(async()=>await b.evaluate("typeof pageInstance==='number'&&document.readyState==='complete'&&pageInstance!=="+JSON.stringify(before))&&await b.evaluate('helloSeen>0'),'admitted code/open mode still connects');
  }
  assert.equal(await b.evaluate("document.querySelector('#token-layer').hidden"),true);
  shots.push(await b.shot('cached-page-connected-'+limited));
  const attempts=(await service.fixture({})).wsAttempts;
  await b.evaluate('new Promise(r=>setTimeout(r,1200))');
  assert.equal((await service.fixture({})).wsAttempts,attempts,'connected page has no automatic retry/reload loop');
  await b.evaluate("document.querySelector('#car-reset-page').click()");
  await b.wait(()=>b.evaluate("new URL(location.href).searchParams.has('cowplay_refresh')"),'explicit cache-bypassing URL');
  await b.wait(()=>b.evaluate("helloSeen>0&&document.querySelector('#token-layer').hidden"),'fresh page connects without new admission');
  await b.wait(async()=>(await service.fixture({})).wsAttempts>attempts,'new viewer channel');
  if(limited)assert.equal(await b.evaluate("typeof VideoDecoder==='undefined'&&typeof AudioDecoder==='undefined'&&typeof navigator.wakeLock==='undefined'"),true);
  shots.push(await b.shot('cache-recovered-'+limited));
  const fresh=await b.evaluate('pageInstance');await b.evaluate('new Promise(r=>setTimeout(r,1200))');assert.equal(await b.evaluate('pageInstance'),fresh,'page reset does not create a reload loop');
  assert.deepEqual(b.errors,[]);assert.deepEqual(b.warnings,[]);
  checks.push({limited,browser:await b.call('Browser.getVersion'),viewport:await b.evaluate('({width:innerWidth,height:innerHeight,dpr:devicePixelRatio})'),pass:true});
 }finally{await b.close();await service.close();}
}
console.log(JSON.stringify({checks,shots,scope:'Chrome with selected APIs removed and synthetic UA/cache; not Tesla or an old Chromium engine'},null,2));
