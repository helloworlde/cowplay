// Real DOM navigation; synthetic receiver state only, never opens a vehicle connection.
import assert from 'node:assert/strict';
import {start} from './lib/ui-browser.mjs';
import {startFixture} from './lib/ui-fixture.mjs';
import {startLuciFixture} from './lib/luci-fixture.mjs';
const service=await startFixture(),b=await start(),checks=[],shots=[];
let luci;
try {
 await service.fixture({networkPatch:{platform:'f50',luci:'https://receiver.example.test:9443/cgi-bin/luci/admin/services/cowplay'}});
 await b.viewport(1280,720);await b.goto(service.origin+'/car#settings/pic');
 await b.wait(()=>b.evaluate("!!document.querySelector('#settings-tab-tesla')"),'native Tesla setting');
 assert.equal(await b.evaluate("document.querySelector('#settings-tab-tesla').tagName"),'BUTTON');
 await b.evaluate("document.querySelector('#settings-tab-tesla').click()");
 await b.wait(()=>b.evaluate("location.hash==='#settings/tesla'&&!!document.querySelector('#tesla-unlock')"),'Tesla opens in the same settings pane');
 assert.equal(await b.evaluate('location.pathname'),'/car');
 assert.equal(await b.evaluate("document.querySelector('#settings-tab-tesla').getAttribute('aria-current')"),'page');
 shots.push(await b.shot('tesla-settings-native-1280'));
 await b.viewport(800,480);assert.ok(await b.evaluate('document.documentElement.scrollWidth<=innerWidth'));shots.push(await b.shot('tesla-settings-native-800'));
 await b.evaluate("document.querySelector('#settings-tab-audio').click()");await b.wait(()=>b.evaluate("!document.querySelector('#tesla-management')&&document.querySelector('.s-pane h1').textContent==='声音与通话'"),'native microphone settings beside Tesla');
 checks.push('Tesla is a native settings tab beside picture and microphone, with inline management unlock and no external navigation');
 if(!process.env.LUCI_RUNTIME_DIR)throw Error('Official LuCI runtime is required');
 luci=await startLuciFixture(process.env.LUCI_RUNTIME_DIR,service.origin);
 await b.viewport(1440,1000);await b.goto(luci.origin+'/luci#overview');
 await b.wait(()=>b.evaluate("!![...document.querySelectorAll('.cw-tabs a')].find(a=>a.textContent==='Tesla 车辆')"),'LuCI Tesla navigation');
 assert.equal(await b.evaluate("[...document.querySelectorAll('.cw-tabs a')].find(a=>a.textContent==='Tesla 车辆').getAttribute('href')"),'/cgi-bin/luci/admin/services/cowplay/tesla');
 shots.push(await b.shot('tesla-luci-link'));checks.push('CowPlay in-page navigation includes a direct Tesla page link');
 assert.deepEqual(b.errors,[]);console.log(JSON.stringify({checks,shots,errors:b.errors},null,2));
} finally {await b.close();if(luci)await luci.close();await service.close();}
