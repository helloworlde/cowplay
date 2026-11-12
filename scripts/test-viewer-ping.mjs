// Execute the real car ping producer without sockets or an artificial RTT implementation.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import vm from 'node:vm';
import ts from '../web/node_modules/typescript/lib/typescript.js';

const source = ts.createSourceFile('main.ts', fs.readFileSync(new URL('../web/src/car/main.ts', import.meta.url), 'utf8'), ts.ScriptTarget.Latest, true);
const declarations = [], reports = [];
function visit(node) {
  if (ts.isVariableStatement(node) && node.declarationList.declarations.some(d => ['ping', 'send'].includes(d.name.getText(source)))) declarations.push(node.getText(source));
  if (ts.isFunctionDeclaration(node) && ['onPong', 'closeSocket'].includes(node.name?.text)) declarations.push(node.getText(source));
  if (ts.isCallExpression(node) && node.expression.getText(source) === 'setInterval' && node.arguments[0]?.getText(source).includes('ping.sent.set')) declarations.push(node.getText(source));
  if (ts.isObjectLiteralExpression(node) && node.properties.some(p => ts.isPropertyAssignment(p) && p.name.getText(source) === 'type' && p.initializer.getText(source) === "'metrics'")) reports.push(node.properties.find(p => p.name?.getText(source) === 'report').initializer.getText(source));
  ts.forEachChild(node, visit);
}
visit(source);
assert.equal(declarations.length, 5, 'extract the existing ping/send/timer/close paths exactly once');
assert.equal(reports.length, 1, 'extract the actual browser metrics report');
let now = 0, tick, closed = false;
const sent = [], context = vm.createContext({performance: {now: () => now}, setInterval: fn => {tick = fn;}, clearTimeout() {}, WebSocket: {OPEN: 1},
  socket: {readyState: 1, bufferedAmount: 8192, send: data => sent.push(JSON.parse(data)), close: () => {closed = true;}},
  app: {channel: true, local: {rttMs: null, jitterMs: null, lossPct: null}}, mic: {stop() {}}, deadline: 0,
  document: {visibilityState: 'visible'}, player: {}, audio: {bufferedMs: () => 0, counters: {underruns: 0, drops: 0}}, bufferS: null,
  st: {fps: 60, decodeMs: null, decoded: 5, dropped: 0, intervals: [], decoder: 'synthetic'}, failed: () => assert.fail('unexpected channel failure')});
const run = code => vm.runInContext(ts.transpileModule(code, {compilerOptions: {target: ts.ScriptTarget.ES2022}}).outputText, context);
run(declarations.join('\n'));
tick(); assert.equal(sent.length, 0); assert.equal(run('ping.sent.size'), 0, 'an unsent congested ping must not become a pending RTT sample');
now = 1000; context.socket.bufferedAmount = 0; tick(); assert.equal(sent.length, 1);
now = 1010; run('onPong()'); assert.equal(run('ping.latest.rtt_ms'), 10); assert.equal(run('ping.sent.size'), 0);
now = 1500; const report = run('(' + reports[0] + ')');
assert.equal(report.ping.seq, 1); assert.equal(report.ping.rtt_ms, 10); assert.equal(report.ping.age_ms, 490);
now = 4500; assert.equal(run('(' + reports[0] + ')').ping.age_ms, 3490, 'repeated metrics retain the real sample age');
run('closeSocket()'); assert.equal(closed, true); assert.equal(run('ping.latest'), null); assert.equal(run('ping.sent.size'), 0);
assert.equal(run('(' + reports[0] + ')').ping, undefined, 'a new channel cannot report the preceding channel pong');
console.log(JSON.stringify({passed: true, checks: ['unsent ping is not registered', 'individual pong RTT', 'sample sequence and increasing age', 'channel close clears the sample']}));

// Replay out-of-order API snapshots through the real statistics polling callback.
const statsTimers=[];
function findStats(node){
  if(ts.isCallExpression(node)&&node.expression.getText(source)==='setInterval'&&node.arguments[0]?.getText(source).includes('await getStats()'))statsTimers.push(node.getText(source));
  ts.forEachChild(node,findStats);
}
findStats(source);assert.equal(statsTimers.length,1);
let statsNow=100,statsTick;const requests=[];
const state={session:{id:'phone-A'},me:{prefs:{debug:true}}};
const statsApp={authorized:true,view:'session',state,stats:null,statsAt:0,statsError:''};
const statsContext=vm.createContext({authorization:1,performance:{now:()=>statsNow},app:statsApp,scene:()=>'',setInterval:fn=>{statsTick=fn},getStats:()=>new Promise(resolve=>requests.push(resolve))});
const statsRun=code=>vm.runInContext(ts.transpileModule(code,{compilerOptions:{target:ts.ScriptTarget.ES2022}}).outputText,statsContext);
statsRun(statsTimers[0]);
const sample=(id,label)=>({session:{id},label});
const old=statsTick();statsNow=1000;const recent=statsTick();statsNow=1100;requests[1](sample('phone-A','new'));await recent;
assert.equal(statsApp.stats.label,'new');
statsNow=1200;requests[0](sample('phone-A','old'));await old;
assert.equal(statsApp.stats.label,'new','a late snapshot cannot overwrite a newer response or restart its age');
assert.equal(statsApp.statsAt,1000,'freshness starts when the request began, conservatively including transit');
statsNow=7100;const previousPhone=statsTick();state.session={id:'phone-B'};requests[2](sample('phone-A','previous-phone'));await previousPhone;
assert.equal(statsApp.stats.label,'new','a response from a previous phone session is ignored');
statsNow=7200;const previousClient=statsTick();statsContext.authorization++;requests[3](sample('phone-B','previous-client'));await previousClient;
assert.equal(statsApp.stats.label,'new','a response from a revoked authorization is ignored');
statsNow=7300;const current=statsTick();requests[4](sample('phone-B','current'));await current;assert.equal(statsApp.stats.label,'current');
statsNow=8000;const expired=statsTick();statsNow=13000;requests[5](sample('phone-B','expired'));await expired;
assert.equal(statsApp.statsAt,7300,'an expired response cannot refresh the hardware age');
console.log(JSON.stringify({passed:true,checks:['late snapshot does not overwrite newer data','request start measures age','phone session isolation','authorization isolation','expired response rejected']}));
