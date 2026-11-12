// Synthetic receiver identities and real Chrome DOM. Never connects to a device.
import assert from 'node:assert/strict';
import {start} from './lib/ui-browser.mjs';
import {startFixture} from './lib/ui-fixture.mjs';
const service=await startFixture(), b=await start(), checks=[],shots=[];
async function load(route,w,h){await b.viewport(w,h);await b.goto(service.origin+route);await b.wait(()=>b.evaluate("document.documentElement.dataset.ready==='true'||!!document.querySelector('#c-title')?.textContent"),'page ready');}
const text=()=>b.evaluate('document.body.innerText');
async function carLabel(device){
  await b.wait(()=>b.evaluate(`document.querySelector('.receiver-node .node-sub')?.textContent===${JSON.stringify(device)}`),'short receiver label');
  assert.equal(await b.evaluate("document.querySelector('.receiver-node .node-label').textContent"),'CowPlay');
  assert.equal(await b.evaluate("getComputedStyle(document.querySelector('.receiver-node .node-label')).whiteSpace"),'nowrap');
  assert.equal(await b.evaluate('document.documentElement.scrollWidth>innerWidth'),false);
}
try{
  await service.fixture({mode:'connecting',auth:true,networkPatch:{platform:'linux',device_name:'rasp',receiver_name:'cowplay-rasp',luci:service.origin+'/admin'}});
  await load('/car',1280,720);await carLabel('rasp');
  assert.ok(!(await text()).includes('F50'));assert.ok(!(await text()).includes('OpenWrt'));
  assert.ok(await b.evaluate("document.querySelector('#c-admin a').getAttribute('href').endsWith('/admin')"));
  shots.push(await b.shot('linux-car-name-1280'));
  await load('/car',390,844);await carLabel('rasp');shots.push(await b.shot('linux-car-name-390'));
  checks.push('Connection diagrams show CowPlay and the short device name on desktop and portrait screens');
  await load('/car#settings/devices',1280,720);
  await b.wait(()=>b.evaluate("[...document.querySelectorAll('[role=tab]')].some(x=>x.textContent.includes('Linux 主机'))"),'Linux device tab');
  await b.evaluate("[...document.querySelectorAll('[role=tab]')].find(x=>x.textContent.includes('Linux 主机')).click()");
  await b.wait(async()=>(await text()).includes('Raspberry Pi 测试'),'host information');
  assert.ok((await text()).includes('Debian 测试'));assert.ok((await text()).includes('cowplay-rasp'));assert.ok(!(await text()).includes('OpenWrt'));
  shots.push(await b.shot('linux-device-info-1280'));
  await load('/phone',390,844);await b.wait(()=>b.evaluate("document.querySelector('.receiver-node b')?.textContent==='rasp'"),'phone device label');
  assert.ok(!(await text()).includes('F50'));assert.equal(await b.evaluate('document.documentElement.scrollWidth>innerWidth'),false);shots.push(await b.shot('linux-phone-name-390'));
  await load('/phone#settings',390,844);await b.wait(async()=>(await text()).includes('CowPlay 主机管理'),'phone Linux management');
  assert.ok(!(await text()).includes('OpenWrt'));shots.push(await b.shot('linux-phone-settings-390'));
  await load('/phone#diag',390,844);await b.wait(async()=>(await text()).includes('Debian 测试'),'phone host OS');assert.ok((await text()).includes('cowplay-rasp'));
  checks.push('Host information shows the actual broadcast name and OS; management text follows the platform');
  await service.fixture({networkPatch:{receiver_name:'cowplay-garage',device_name:'garage'}});
  await b.wait(async()=>(await text()).includes('cowplay-garage'),'changed receiver name');assert.ok(!(await text()).includes('cowplay-rasp'));
  // A malformed/legacy long broadcast name must never become the main diagram label.
  await service.fixture({networkPatch:{receiver_name:'车库接收端'.repeat(3)+'-rasp-02A6325FB440',device_name:'rasp'}});
  await load('/car',390,844);await carLabel('rasp');assert.ok(!(await text()).includes('车库接收端'));shots.push(await b.shot('legacy-name-short-diagram-390'));
  await service.fixture({networkPatch:{platform:'f50',receiver_name:'cowplay-f50',device_name:'f50',luci:service.origin+'/luci'}});
  await load('/car',1280,720);await carLabel('f50');
  await load('/car#settings/devices',1280,720);await b.wait(()=>b.evaluate("[...document.querySelectorAll('[role=tab]')].some(x=>x.textContent.includes('F50'))"),'F50 device tab');
  checks.push('Changed devices replace old names; long broadcast names stay out of diagrams; F50 management remains correct');
  assert.deepEqual(b.errors,[]);console.log(JSON.stringify({checks,shots,errors:b.errors},null,2));
}catch(e){console.error(e.stack);throw e;}finally{await b.close();await service.close();}
