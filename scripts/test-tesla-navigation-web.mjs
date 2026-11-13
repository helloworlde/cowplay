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
 await b.wait(()=>b.evaluate("!!document.querySelector('#settings-tesla')"),'Tesla settings link');
 assert.deepEqual(await b.evaluate("(()=>{const a=document.querySelector('#settings-tesla');return [a.href,a.target,a.textContent]})()"),['https://receiver.example.test:9443/cgi-bin/luci/admin/services/cowplay/tesla','_blank','Tesla 车辆配对与连接 ↗']);
 shots.push(await b.shot('tesla-settings-link-1280'));
 await b.viewport(800,480);assert.ok(await b.evaluate('document.documentElement.scrollWidth<=innerWidth'));shots.push(await b.shot('tesla-settings-link-800'));
 checks.push('car settings links directly to Tesla management and preserves the advertised HTTPS port');
 await service.fixture({networkPatch:{platform:'linux',luci:'https://receiver.example.test:18443/manage'}});
 await b.wait(()=>b.evaluate("document.querySelector('#settings-tesla')?.href==='https://receiver.example.test:18443/manage#tesla-management'"),'Linux Tesla route');
 checks.push('Linux settings links directly to the Tesla management section');
 if(!process.env.LUCI_RUNTIME_DIR)throw Error('Official LuCI runtime is required');
 luci=await startLuciFixture(process.env.LUCI_RUNTIME_DIR,service.origin);
 await b.viewport(1440,1000);await b.goto(luci.origin+'/luci#overview');
 await b.wait(()=>b.evaluate("!![...document.querySelectorAll('.cw-tabs a')].find(a=>a.textContent==='Tesla 车辆')"),'LuCI Tesla navigation');
 assert.equal(await b.evaluate("[...document.querySelectorAll('.cw-tabs a')].find(a=>a.textContent==='Tesla 车辆').getAttribute('href')"),'/cgi-bin/luci/admin/services/cowplay/tesla');
 shots.push(await b.shot('tesla-luci-link'));checks.push('CowPlay in-page navigation includes a direct Tesla page link');
 assert.deepEqual(b.errors,[]);console.log(JSON.stringify({checks,shots,errors:b.errors},null,2));
} finally {await b.close();if(luci)await luci.close();await service.close();}
