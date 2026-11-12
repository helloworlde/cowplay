// Real DOM, synthetic authorization responses; no device or private cookies.
import assert from 'node:assert/strict';
import {start} from './lib/ui-browser.mjs';
import {startFixture} from './lib/ui-fixture.mjs';

const results=[];
async function scenario(name, source, run) {
  const service=await startFixture(), browser=await start();
  try {
    await service.fixture({auth:true,mode:'connecting',stopped:true});
    await browser.call('Page.addScriptToEvaluateOnNewDocument',{source});
    await browser.viewport(1280,720);
    await browser.goto(service.origin+'/car');
    await run(browser,service);
    assert.deepEqual(browser.errors,[]);
    results.push({name,pass:true});
  } catch(error) {
    results.push({name,pass:false,error:String(error),browserErrors:browser.errors,
      view:await browser.evaluate("({view:document.body.dataset.view,painted:window.painted,cover:document.querySelector('#cover-text')?.textContent})").catch(()=>null)});
  }
  finally {await browser.close();await service.close();}
}

await scenario('valid authorization and rejected state must not restart admission', `
  window.authReads=0;window.authPosts=0;window.denyState=true;
  const original=window.fetch;
  window.fetch=(url,init)=>{
    if(url==='/api/auth')init?.method==='POST'?authPosts++:authReads++;
    if(url==='/api/state'&&window.denyState)return Promise.resolve(new Response('{}',{status:401}));
    return original(url,init);
  };
`,async(b,s)=>{
  await b.wait(()=>b.evaluate('authReads>=2'),'authorization recheck');
  await b.evaluate('new Promise(r=>setTimeout(r,300))');
  const counts=await b.evaluate('({reads:authReads,posts:authPosts})');
  assert.ok(counts.reads<=2,`recursive auth loop: ${JSON.stringify(counts)}`);
  assert.equal(counts.posts,0);
  assert.equal(await b.evaluate("document.querySelector('#token-layer').hidden"),true);
  await b.evaluate('window.denyState=false');
  await b.wait(async()=>(await s.fixture({})).wsAttempts>0,'state recovers without another token');
});

await scenario('revoked socket with valid cookie must keep the page authorized','',async(b,s)=>{
  await b.wait(async()=>(await s.fixture({})).wsAttempts>0,'socket');
  await b.evaluate(`window.tokenFlashes=0;new MutationObserver(()=>{if(!document.querySelector('#token-layer').hidden)tokenFlashes++}).observe(document.querySelector('#token-layer'),{attributes:true,attributeFilter:['hidden']})`);
  await s.fixture({wsText:{type:'error',code:'AUTH_REVOKED'}});
  await b.wait(async()=>(await s.fixture({})).wsAttempts>1,'bounded socket recovery');
  assert.equal(await b.evaluate('tokenFlashes'),0);
});

await scenario('genuine revocation shows a stable keypad and explicit token restores access','',async(b,s)=>{
  await b.wait(async()=>(await s.fixture({})).wsAttempts>0,'socket');
  await s.fixture({auth:false,wsText:{type:'error',code:'AUTH_REVOKED'}});
  await b.wait(()=>b.evaluate("!document.querySelector('#token-layer').hidden"),'revoked keypad');
  await b.evaluate("for(const n of '123456')[...document.querySelectorAll('#keys button')].find(x=>x.textContent===n).click()");
  await b.wait(()=>b.evaluate("document.querySelector('#token-layer').hidden"),'explicit reauthorization');
});

await scenario('a failed authorization recheck freezes media then recovers without a keypad',`
  window.failAuthCheck=false;window.painted=0;
  const original=window.fetch;
  window.fetch=(url,init)=>{
    if(url==='/api/auth'&&window.failAuthCheck){window.failAuthCheck=false;return Promise.resolve(new Response('{}',{status:503}));}
    return original(url,init);
  };
  const pixels=new OffscreenCanvas(2,2);
  pixels.getContext('2d').fillRect(0,0,2,2);
  window.VideoDecoder=class {
    state='unconfigured';decodeQueueSize=0;
    constructor(init){this.init=init;}
    configure(){this.state='configured';}
    decode(c){if(this.state==='configured'){this.init.output(new VideoFrame(pixels,{timestamp:c.timestamp}));window.painted++;}}
    close(){this.state='closed';}
    static async isConfigSupported(config){return {supported:true,config};}
  };
`,async(b,s)=>{
  await s.fixture({mode:'session',stopped:false});
  await b.wait(()=>b.evaluate("document.body.dataset.view==='session'&&painted>0"),'rendered media');
  const before=(await s.fixture({})).wsAttempts;
  await b.evaluate('window.failAuthCheck=true');
  await s.fixture({wsText:{type:'error',code:'AUTH_REVOKED'}});
  await b.wait(()=>b.evaluate("document.body.classList.contains('stalled')"),'old media invalidated');
  assert.equal(await b.evaluate("document.querySelector('#token-layer').hidden"),true);
  await b.wait(async()=>(await s.fixture({})).wsAttempts>before,'socket recovers after transient verification failure');
  await b.wait(()=>b.evaluate("document.body.dataset.view==='session'&&!document.body.classList.contains('stalled')"),'new frames restore view');
});

await scenario('build mismatch keeps authorization and waits for an explicit refresh',
  'window.pageInstance=crypto.randomUUID()',async(b,s)=>{
  await b.wait(async()=>(await s.fixture({})).wsAttempts>0,'socket');
  const page=await b.evaluate('pageInstance');
  await s.fixture({wsText:{type:'error',code:'CLIENT_BUILD_MISMATCH'}});
  await b.wait(()=>b.evaluate("[...document.querySelectorAll('#c-body button')].some(b=>b.textContent==='刷新页面')"),'refresh action');
  assert.equal(await b.evaluate('pageInstance'),page);
  assert.equal(await b.evaluate("document.querySelector('#token-layer').hidden"),true);
  await b.evaluate("[...document.querySelectorAll('#c-body button')].find(b=>b.textContent==='刷新页面').click()");
  await b.wait(()=>b.evaluate(`pageInstance!==${JSON.stringify(page)}`),'user refresh');
  await b.wait(()=>b.evaluate("document.querySelector('#token-layer').hidden"),'authorization survives reload');
});

await scenario('a token ACK without a saved cookie cannot flash an authorized page',`
  const original=window.fetch;
  window.fetch=(url,init)=>url==='/api/auth'&&init?.method!=='POST'
    ? Promise.resolve(new Response(JSON.stringify({authorized:false,required:true,locked_s:null}),{headers:{'Content-Type':'application/json'}}))
    : original(url,init);
`,async(b)=>{
  await b.wait(()=>b.evaluate("!document.querySelector('#token-layer').hidden"),'keypad');
  await b.evaluate("for(const n of '123456')[...document.querySelectorAll('#keys button')].find(x=>x.textContent===n).click()");
  await b.wait(()=>b.evaluate("document.querySelector('#token-msg').textContent.includes('浏览器未保存授权')"),'cookie persistence error');
  assert.equal(await b.evaluate("document.querySelector('#token-layer').hidden"),false);
});

await scenario('PNG and all initial page resources load from the bundled origin','',async(b,s)=>{
  await b.wait(()=>b.evaluate("document.querySelector('.app-icon img')?.complete"),'bundled icon');
  const resource=await b.evaluate(`({width:document.querySelector('.app-icon img').naturalWidth,external:performance.getEntriesByType('resource').filter(r=>new URL(r.name).origin!==location.origin).map(r=>r.name)})`);
  assert.ok(resource.width>0);assert.deepEqual(resource.external,[]);
});

console.log(JSON.stringify({synthetic:true,results},null,2));
if(results.some(r=>!r.pass))process.exitCode=1;
