// Synthetic management API and real Chrome DOM; never connects to BlueZ or a device.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import http from 'node:http';
import path from 'node:path';
import {start} from './lib/ui-browser.mjs';
const root=path.resolve(import.meta.dirname,'../web/dist'), token='ab'.repeat(32), calls=[];
let accessCode='123456';
let policy={max_res:'1080p',max_fps:60,auto_downgrade:false};
const reply=(res,status,body)=>{res.writeHead(status,{'content-type':'application/json'});res.end(JSON.stringify(body));};
const server=http.createServer(async(req,res)=>{
  const url=new URL(req.url,origin);const bytes=[];for await(const c of req)bytes.push(c);
  if(url.pathname.startsWith('/api/manage/')) {
    const args=JSON.parse(Buffer.concat(bytes).toString()||'{}');
    calls.push({method:url.pathname.split('/').at(-1),args,origin:req.headers.origin,cookie:req.headers.cookie});
    if(req.headers.authorization!==`Bearer ${token}`)return reply(res,401,{error:'管理令牌无效'});
    if(req.headers.origin!==origin)return reply(res,403,{error:'来源不一致'});
    if(url.pathname.endsWith('/status'))return reply(res,200,{version:'synthetic',build:'linux-fixture',policy,verdict:'等待 iPhone',network:{origin,cert_days:30}});
    if(url.pathname.endsWith('/policy_update')) {
      if(!['1080p','1280x800'].includes(args.max_res))return reply(res,409,{error:'分辨率超出接收端范围'});
      policy={...args};return reply(res,200,{policy});
    }
    if(url.pathname.endsWith('/token'))return reply(res,200,{token:accessCode,mode:accessCode?'code':'open'});
    if(url.pathname.endsWith('/token_disable')){accessCode='';return reply(res,200,{ok:true});}
    if(url.pathname.endsWith('/token_set')){accessCode=args.token;return reply(res,200,{ok:true});}
    return reply(res,404,{error:'未实现的合成方法'});
  }
  const file=path.join(root,url.pathname==='/manage'?'manage.html':url.pathname);
  if(!file.startsWith(root+'/')){res.writeHead(404);return res.end();}
  try{const body=fs.readFileSync(file);res.writeHead(200,{'content-type':file.endsWith('.js')?'text/javascript':file.endsWith('.css')?'text/css':'text/html'});res.end(body);}catch{res.writeHead(404);res.end();}
});
await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve));
const origin=`http://127.0.0.1:${server.address().port}`, b=await start(), checks=[],shots=[];
const click=label=>b.evaluate(`[...document.querySelectorAll('button')].find(b=>b.textContent===${JSON.stringify(label)}).click()`);
const value=(label,v)=>b.evaluate(`document.querySelector('[aria-label=${JSON.stringify(label)}]').value=${JSON.stringify(v)}`);
try {
  await b.viewport(1280,900);await b.goto(origin+'/manage');
  assert.equal(await b.evaluate('document.title'),'CowPlay 管理');
  await value('管理令牌','0'.repeat(64));await b.evaluate("document.querySelector('form').requestSubmit()");
  await b.wait(()=>b.evaluate("document.querySelector('#feedback').textContent.includes('管理令牌无效')"),'wrong token');
  assert.equal(await b.evaluate("document.querySelector('[aria-label=管理令牌]').value"),'');
  await value('管理令牌',token);await b.evaluate("document.querySelector('form').requestSubmit()");
  await b.wait(()=>b.evaluate("document.body.textContent.includes('画面上限')"),'management content');
  checks.push('page identity, failed login, cleared password, management status and no blank page');
  await value('分辨率上限','custom');await value('自定义分辨率','1280x800');await value('帧率上限','30');await click('保存画面上限');
  await b.wait(()=>b.evaluate("!!document.querySelector('dialog[open]')"),'save confirmation');await click('保存并应用');
  await b.wait(()=>b.evaluate("document.querySelector('#feedback').textContent.includes('已保存并应用')"),'saved policy');
  assert.equal(policy.max_res,'1280x800');assert.equal(policy.max_fps,30);
  await value('分辨率上限','custom');await value('自定义分辨率','99999x800');await click('保存画面上限');await b.wait(()=>b.evaluate("!!document.querySelector('dialog[open]')"),'invalid save confirmation');await click('保存并应用');
  await b.wait(()=>b.evaluate("document.querySelector('#feedback').textContent.includes('分辨率超出')"),'rejected policy');assert.equal(policy.max_res,'1280x800');
  checks.push('confirmation, persisted selection and invalid-save recovery preserve current policy');
  shots.push(await b.shot('linux-management-error-1280'));
  await value('分辨率上限','custom');await value('自定义分辨率','1280x800');
  await click('显示访问码');await b.wait(()=>b.evaluate("document.querySelector('#screen-token').textContent==='123456'"),'screen token');
  await click('不使用访问码');await b.wait(()=>b.evaluate("!!document.querySelector('dialog[open]')"),'disable confirmation');await click('取消');await b.wait(()=>b.evaluate("!document.querySelector('dialog')&&![...document.querySelectorAll('button')].find(b=>b.textContent==='不使用访问码').disabled"),'cancel completes and action is available');assert.equal(accessCode,'123456');
  await click('不使用访问码');await b.wait(()=>b.evaluate("!!document.querySelector('dialog[open]')"),'disable confirmation again');await click('停用访问码');
  await b.wait(()=>b.evaluate("document.querySelector('#screen-token').textContent==='未启用访问码'"),'disabled code');assert.equal(accessCode,'');
  await value('新访问码','234567');await click('设置访问码');await b.wait(()=>b.evaluate("!!document.querySelector('dialog[open]')"),'enable confirmation');await click('设置');
  await b.wait(()=>b.evaluate("document.querySelector('#screen-token').textContent==='已设置'"),'enabled code');assert.equal(accessCode,'234567');
  checks.push('optional code disable cancel, explicit disable and reenable');
  await click('替换 HTTPS 证书');await b.wait(()=>b.evaluate("document.querySelector('#feedback').textContent.includes('请选择证书链和私钥')"),'missing certificate');
  assert.ok(calls.every(c=>c.origin===origin&&!c.cookie));
  assert.equal(await b.evaluate('localStorage.length+sessionStorage.length'),0);assert.equal(await b.evaluate('location.href'),origin+'/manage');
  shots.push(await b.shot('linux-management-1280'));
  await b.viewport(390,844);assert.ok(await b.evaluate('document.documentElement.scrollWidth<=innerWidth'));shots.push(await b.shot('linux-management-390'));
  await click('退出管理');assert.ok(await b.evaluate("!!document.querySelector('[aria-label=管理令牌]')"));
  assert.deepEqual(b.errors,[]);checks.push('certificate selection, token display, secret kept out of storage/URL, same-origin requests, logout and mobile layout');
  console.log(JSON.stringify({browser:'Browser plugin not available; existing Chrome/CDP regression helper',checks,shots,errors:b.errors},null,2));
}finally{await b.close();await new Promise(resolve=>server.close(resolve));}
