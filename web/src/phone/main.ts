import {receiverName} from '../shared/receiver';
import {confirmAction,cancelConfirmation,preferenceChanges,preferencePrompt,commandPrompt,stateContext} from '../shared/confirmation';
import {$, clear, h, icon} from '../shared/dom';
import {ApiError, admit, api, authStatus, setupAccess, download, getDiag, getState, getStats, pairingAdapterName, pairStage, recentLinkError, runSegments, saveError, sendCmd, setPrefs, targetPhone, type PairStage, type ScreenPrefs} from '../shared/api';
import {mmss} from '../shared/fmt';
import {browserName, toast} from '../car/ui';
import {NearbyPhones, PairingConfirmation} from '../shared/pairing';
import {PictureEditor} from '../shared/picture-editor';
import {ConnectionDebug} from '../shared/connection-debug';
import {app, prefs, targetScreen, type Actions, type Page} from './app';
import {diagnose, iphones, logs, settings, status} from './pages';
import {button, foot, group, text} from './ui';

let pairing = location.pathname === '/pair' || location.pathname.endsWith('/pair/');
let authEpoch=0,uiEpoch=0;
let pollBusy = false, writingPrefs = false, refreshEpoch = 0, unlocked = false;
let pollTimer = 0;
const pictureEditor=new PictureEditor();
const pendingCommands = new Map<string,object>();
let pendingPrefs:object|null=null;
let tokenRequest=0,logRequest=0;
let pendingPolicy:object|null=null,pendingTest:object|null=null;
let pendingPairCancel:object|null=null;
function resetPairCancel():void{pendingPairCancel=null;app.pairCancelBusy=false;app.pairCancelError='';}
function hideToken():void{tokenRequest++;app.token=null;app.showToken=false;app.tokenBusy=false;}
function clearSegments():void{pendingTest=null;app.busy=false;app.segments=null;if(app.error.startsWith('连接检查失败：'))app.error='';}
function segmentContext():string{const s=app.state;return JSON.stringify([authEpoch,s?.me.id,s?.current_phone,targetPhone(s)?.id,s?.session?.id,s?.viewer?.id,s?.hub.phone,s?.pairing.generation,s?.pairing.device]);}
function accessChanged():void{cancelConfirmation();authEpoch++;uiEpoch++;refreshEpoch++;pendingCommands.clear();pendingPrefs=null;writingPrefs=false;app.prefsBusy=false;pendingPolicy=null;app.policyBusy=false;resetPairCancel();hideToken();clearSegments();}
let pairingAvailable = false;
let panelTrigger: HTMLElement | null = null;
const pairingConfirmation = new PairingConfirmation(h('section', {class: 'code-card'}), 'phone-',
  body => api('/api/pairing/confirm', {body}), poll, undefined, () => app.authorized ? app.state?.me.id ?? '' : '');
const nearbyPhones = new NearbyPhones(poll, render, () => app.authorized ? app.state?.me.id ?? '' : '');
const debug=new ConnectionDebug(()=>app.authorized?app.state?.me.id??'':'');
const brandIcon=new URL('../shared/cowplay-icon.png',import.meta.url).href;
const names = {status: 'CowPlay', iphones: 'iPhone', diag: '诊断', log: '日志', settings: '设置'};
const pages = {status, iphones, diag: diagnose, log: logs, settings};
const nav = [['status', '状态', 'monitor'], ['iphones', 'iPhone', 'phone'], ['diag', '诊断', 'activity'], ['settings', '设置', 'sliders']] as const;

function route(): void {
  uiEpoch++;cancelConfirmation();
  const hash = location.hash.slice(1);
  app.page = ['status', 'iphones', 'diag', 'log', 'settings'].includes(hash) ? hash as Page : 'status';
  hideToken(); resetPairCancel(); app.sheet = null;
  render();
  if (app.page === 'log') void loadLogs();
}
window.addEventListener('hashchange', route);
function render(): void {
  if (!app.authorized) return;
  const focused = document.activeElement as HTMLElement | null;
  if (app.state) pairingConfirmation.update(app.state.pairing, !pairingAvailable);
  document.body.classList.toggle('pair-page', pairing);
  if (pairing) { renderPair(); if (focused?.isConnected && focused !== document.body) focused.focus({preventScroll: true}); return; }
  const root = $('content'), keep = window.scrollY;
  const top = h('div', {class: 'nav-row'});
  if (app.page === 'iphones') top.append(h('span', {class: 'grow'}), h('button', {type: 'button', onclick: () => { app.edit = !app.edit; render(); }}, app.edit ? '完成' : '编辑'));
  if (app.page === 'log') top.append(h('a', {href: '#diag'}, '‹ 诊断'), h('span', {class: 'grow'}), h('button', {type: 'button', onclick: () => void exportLogs()}, '分享'));
  const head = h('h1', {}, names[app.page]);
  clear(root, ...(app.page === 'iphones' || app.page === 'log' ? [top] : []), head,
    app.error ? h('p', {class: 'error', role: 'alert'}, app.error) : null,
    ...(app.state ? pages[app.page](actions) : [foot('正在读取设备状态…')]), app.state&&(app.page==='status'||app.page==='iphones')?debug.update(app.state):null);
  $('tabs').hidden = false;
  clear($('tabs'), ...nav.map(([id, title, glyph]) => h('button', {type: 'button', id: `phone-tab-${id}`, 'aria-label': title, 'aria-current': (app.page === id || (app.page === 'log' && id === 'diag')) ? 'page' : 'false', onclick: () => { location.hash = id; }}, icon(glyph, 22), h('span', {}, title))));
  renderOverlay();
  if (focused?.isConnected && focused !== document.body && (!app.sheet || $('overlay').contains(focused))) focused.focus({preventScroll: true});
  window.scrollTo(0, keep);
  document.documentElement.dataset.ready = 'true';
}
async function poll(): Promise<void> {
  if (!app.authorized || pollBusy || writingPrefs || document.visibilityState === 'hidden') return;
  pollBusy = true; const epoch = refreshEpoch;
  try {
    const results = await Promise.allSettled([getState(), getStats(), getDiag(),
      app.system ? Promise.resolve(app.system) : api<NonNullable<typeof app.system>>('/api/system'),
      app.page === 'diag' ? api<{warnings: number}>('/api/logs/summary?limit=1') : Promise.resolve(null)]);
    if (epoch !== refreshEpoch) return;
    if (results[0].status === 'rejected') throw results[0].reason;
    const previousPair = app.state?.pairing, previousSegments=segmentContext();
    app.state = results[0].value;
    if(previousPair?.generation!==app.state.pairing.generation){if(pendingPairCancel)uiEpoch++;resetPairCancel();}
    if(previousSegments!==segmentContext())clearSegments();
    if (!pairing && app.state.pairing.request != null && (previousPair?.generation !== app.state.pairing.generation || previousPair?.request !== app.state.pairing.request)) app.sheet = 'add';
    pairingAvailable = true;
    if (app.error.startsWith('暂时连不上 CowPlay')) app.error = '';
    app.stats=results[1].status==='fulfilled'?results[1].value:null;
    if (results[2].status === 'fulfilled') app.diag = results[2].value;
    if (results[3].status === 'fulfilled') app.system = results[3].value;
    if (results[4].status === 'fulfilled' && results[4].value) app.warnings = results[4].value.warnings;
    render();
  } catch (e) {
    pairingAvailable = false; pairingConfirmation.unavailable();
    if (e instanceof ApiError && e.status === 401) { accessChanged(); app.authorized = false; pictureEditor.reset(); app.state=null; debug.reset(); app.token = null; app.showToken = false; await boot(); }
    else { app.error = `暂时连不上 CowPlay：${(e as Error).message}。正在重试…`; render(); }
  } finally { pollBusy = false; }
}
const uiContext=()=>JSON.stringify([uiEpoch,location.hash,app.sheet,app.pref,app.edit]);
const interactionContext=()=>JSON.stringify([authEpoch,location.hash,app.sheet,app.pref,targetScreen()?.id,stateContext(app.state)]);
const actions: Actions = {
  render,
  async cmd(cmd, id) {
    const owner=app.state?.me.id,context=interactionContext(),current=()=>app.authorized&&interactionContext()===context,prompt=app.state&&commandPrompt(cmd,app.state,id);
    if(prompt&&!await confirmAction({...prompt,current}))return false;
    if(!current()||app.state?.me.id!==owner)return false;
    const key = `${cmd}/${id ?? ''}${cmd==='closePairWindow'?`/${app.state?.pairing.generation}`:''}`, access = app.state?.me.id;
    if (pendingCommands.has(key)) return false;
    const token={};pendingCommands.set(key,token);
    const accessEpoch=authEpoch,sourceUI=uiContext(),epoch = ++refreshEpoch;
    try {
      const r = await sendCmd(cmd,id,AbortSignal.timeout(15000),cmd==='closePairWindow'?app.state?.pairing.generation:undefined);
      if (sourceUI!==uiContext()||accessEpoch!==authEpoch||pendingCommands.get(key)!==token||epoch !== refreshEpoch || !app.authorized || app.state?.me.id !== access) return false;
      if (cmd === 'reloadCar' && r?.ok === false) throw new Error('还没有车机屏幕连接，未发送刷新命令');
      if(cmd==='closePairWindow'&&r?.ok!==true)throw new Error('配对取消尚未得到确认，请核对状态后重试');
      app.error = '';
      toast(cmd === 'disconnect' ? 'CarPlay 已断开，配对已保留' : cmd === 'reloadCar' ? '已发送，车机页面正在刷新…' : cmd === 'setPreferred' ? '已设为首选，下次自动连接时优先使用' : cmd === 'forgetPhone' ? '已忘记这台 iPhone' : cmd === 'openPairWindow' ? '配对窗口已打开，请在新 iPhone 上确认' : cmd === 'closePairWindow' ? '配对已取消，已保存的配对保留' : '已发送，正在连接 iPhone…');
      await poll(); return pendingCommands.get(key)===token&&epoch===refreshEpoch&&sourceUI===uiContext()&&accessEpoch===authEpoch&&app.authorized&&app.state?.me.id===access;
    } catch (e) { if (sourceUI===uiContext()&&accessEpoch===authEpoch&&pendingCommands.get(key)===token&&epoch === refreshEpoch && app.authorized && app.state?.me.id === access) { const uncertain=e instanceof DOMException&&/Timeout|Abort/.test(e.name);app.error=uncertain?'操作结果未确认：请求超时，请核对连接状态后重试':(e as Error).message;render();if(uncertain)void poll(); } return false; }
    finally { if(pendingCommands.get(key)===token)pendingCommands.delete(key); }
  },
  async prefs(patch:Partial<ScreenPrefs>,screenId=targetScreen()?.id) {
    if(writingPrefs){toast('设置正在保存，请稍候');return false;}
    if(!prefs()||!screenId)return false;
    if(Object.entries(patch).every(([key,value])=>prefs()![key as keyof ScreenPrefs]===value))return true;
    const access=app.state?.me.id,context=interactionContext(),base=JSON.stringify(prefs()),changes=preferenceChanges(prefs()!,patch);
    const valid=()=>app.authorized&&app.state?.me.id===access&&interactionContext()===context&&targetScreen()?.id===screenId&&JSON.stringify(prefs())===base;
    if(changes.length){
      if(!await confirmAction({...preferencePrompt(changes,true),current:valid}))return false;
      if(!valid()){app.error='车机屏幕、设置或授权已变化，请重新调整';render();return false;}
    }
    if(writingPrefs)return false;
    const token={},accessEpoch=authEpoch,sourceUI=uiEpoch,sheet=app.sheet,pref=app.pref,owner=app.state?.me.id,epoch=++refreshEpoch;pendingPrefs=token;writingPrefs=true;app.prefsBusy=true;render();
    const sameView=()=>sourceUI===uiEpoch&&accessEpoch===authEpoch&&app.authorized&&app.state?.me.id===owner&&targetScreen()?.id===screenId&&app.sheet===sheet&&app.pref===pref;
    const current=()=>pendingPrefs===token&&sameView()&&epoch===refreshEpoch;
    let saved=false;
    try {const result=await setPrefs(patch,'viewer',AbortSignal.timeout(5000),screenId);
      if(!current())return false;
      const target=targetScreen();if(target)target.prefs=result.prefs;app.error='';toast(changes.length?'已保存到车机屏幕，已请求重新连接 CarPlay':'已保存到车机屏幕');saved=true;
    }catch(e){if(current())app.error=saveError(e);return false;}
    finally{if(pendingPrefs===token){pendingPrefs=null;writingPrefs=false;app.prefsBusy=false;await poll();render();}}
    return saved&&sameView();
  },
  async autoConnect(ask) {
    if(app.policyBusy||!app.authorized||!app.state)return false;
    const token={},access=authEpoch,owner=app.state.me.id,sourceUI=uiContext();pendingPolicy=token;app.policyBusy=true;render();
    const current=()=>pendingPolicy===token&&access===authEpoch&&app.authorized&&app.state?.me.id===owner&&sourceUI===uiContext();
    try {
      const result=await api<{ask_when_multi:boolean}>('/api/policy',{body:{ask_when_multi:ask},signal:AbortSignal.timeout(5000)});
      if(!current())return false;
      app.state!.policy.ask_when_multi=result.ask_when_multi;app.error='';render();return true;
    } catch(e) {if(current()){app.error='自动连接设置'+saveError(e);render();}return false;}
    finally {if(pendingPolicy===token){pendingPolicy=null;app.policyBusy=false;await poll();render();}}
  },
  add() {
    void actions.cmd('openPairWindow').then(ok=>{if(ok){app.sheet='add';render();}});
  },
  async close() {
    if(app.pairCancelBusy)return;
    const stage=app.state?pairStage(app.state):'start';
    if(app.sheet==='add'&&['waiting','code','finishing'].includes(stage)){await cancelPairing(true);return;}
    closePanel();
  },
  async test() {
    if (app.busy) return; app.busy = true; app.segments=null;if(app.error.startsWith('连接检查失败：'))app.error='';render();
    const token={},context=segmentContext(),sourceUI=uiContext();pendingTest=token;
    const current=()=>pendingTest===token&&app.authorized&&context===segmentContext()&&sourceUI===uiContext();
    try {const result=await runSegments();if(current()){app.segments=result;app.error='';}}
    catch (e) {if(current())app.error=`连接检查失败：${(e as Error).message}`;}
    finally {if(pendingTest===token){pendingTest=null;app.busy=false;render();}}
  },
  export() {void exportFile('/api/diag/bundle','诊断包已导出');},
  logs() { void loadLogs(); },
  async token() {
    if (app.showToken) {hideToken();render();return;}
    if(app.tokenBusy||!app.authorized||app.page!=='settings')return;
    const request=++tokenRequest,access=authEpoch,sourceUI=uiEpoch,owner=app.state?.me.id;app.tokenBusy=true;render();
    const current=()=>request===tokenRequest&&access===authEpoch&&sourceUI===uiEpoch&&app.authorized&&app.state?.me.id===owner&&app.page==='settings'&&document.visibilityState==='visible';
    try {const r=await api<{token:string}>('/api/token');if(current()){app.token=r.token;app.showToken=true;}}
    catch(e){if(current())app.error=(e as Error).message;}
    finally{if(request===tokenRequest){app.tokenBusy=false;render();}}
  },
};
function closePanel():void{uiEpoch++;cancelConfirmation();resetPairCancel();pictureEditor.reset();app.sheet=null;app.remove=null;unlocked=false;render();const target=panelTrigger?.isConnected?panelTrigger:panelTrigger?.id?document.getElementById(panelTrigger.id):null;target?.focus({preventScroll:true});panelTrigger=null;}
async function cancelPairing(closePanelAfter:boolean):Promise<void>{
  if(app.pairCancelBusy||!app.state||!app.authorized)return;
  const token={},source=uiContext(),access=authEpoch,owner=app.state.me.id,generation=app.state.pairing.generation;
  pendingPairCancel=token;app.pairCancelBusy=true;app.pairCancelError='';render();
  const current=()=>pendingPairCancel===token&&source===uiContext()&&access===authEpoch&&app.authorized&&app.state?.me.id===owner&&app.state.pairing.generation===generation;
  try{
    const accepted=await actions.cmd('closePairWindow');
    if(!current())return;
    if(accepted){if(closePanelAfter)closePanel();}
    else app.pairCancelError=app.error||'配对取消尚未确认，请重试。';
  }finally{if(pendingPairCancel===token){pendingPairCancel=null;app.pairCancelBusy=false;render();}}
}
async function loadLogs(): Promise<void> {
  const view = app.log, level = app.level,request=++logRequest,access=authEpoch,owner=app.state?.me.id,sourceUI=uiContext();
  const current=()=>request===logRequest&&access===authEpoch&&app.authorized&&app.state?.me.id===owner&&sourceUI===uiContext()&&app.page==='log'&&view===app.log&&level===app.level;
  try {
    if (view === 'summary') {const r=await api<{events:typeof app.events}>('/api/logs/summary');if(current())app.events=r.events;}
    else {const r=await api<{entries:typeof app.lines}>(`/api/logs?limit=300${level==='all'?'':`&level=${level}`}`);if(current())app.lines=r.entries;}
    if(current()){if(app.error.startsWith('日志读取失败：'))app.error='';render();}
  } catch(e){if(current()){app.error=`日志读取失败：${(e as Error).message}`;render();}}
}
async function exportFile(path:string,notice:string):Promise<void>{
  const access=authEpoch,owner=app.state?.me.id,sourceUI=uiContext(),failure=path==='/api/diag/bundle'?'诊断包导出失败：':'日志导出失败：';
  const current=()=>access===authEpoch&&app.authorized&&app.state?.me.id===owner&&sourceUI===uiContext();
  try{await download(path);if(current()){if(app.error.startsWith(failure))app.error='';render();toast(notice);}}catch(e){if(current()){app.error=failure+(e as Error).message;render();}}
}
async function exportLogs():Promise<void>{await exportFile('/api/logs/download','日志已导出');}
function modal(title: string, content: (Node | null)[], close = actions.close, closeLabel='取消', closing=false): void {
  if (!$('overlay').querySelector('.bottom-sheet')) panelTrigger = document.activeElement as HTMLElement | null;
  const dim = h('button', {type: 'button', class: 'dim', 'aria-label': '关闭面板', disabled:closing, onclick: close});
  const sheet = h('section', {class: 'bottom-sheet', role: 'dialog', 'aria-modal': 'true', 'aria-label': title}, h('span', {class: 'grabber'}), h('div', {class: 'sheet-head'}, h('h2', {}, title), h('button', {type: 'button', id:'phone-panel-close', disabled:closing, onclick: close}, closeLabel)), ...content);
  sheet.addEventListener('keydown', e => {
    if (e.key !== 'Tab') return;
    const targets = [...sheet.querySelectorAll<HTMLElement>('button,input,select,textarea,a[href],summary,[tabindex]')].filter(el => el.tabIndex >= 0 && !el.matches(':disabled') && el.getClientRects().length);
    const first = targets[0], last = targets.at(-1);
    if ((e.shiftKey && document.activeElement === first) || (!e.shiftKey && document.activeElement === last)) { e.preventDefault(); (e.shiftKey ? last : first)?.focus(); }
  });
  clear($('overlay'), dim, sheet);
  if (!sheet.contains(document.activeElement) && !document.querySelector('.action-confirmation[open]')) sheet.querySelector<HTMLElement>('button:not(:disabled)')?.focus({preventScroll:true});
}
function renderOverlay(): void {
  if (unlocked && !app.sheet) return;
  const previous = $('overlay').querySelector<HTMLElement>('.bottom-sheet'), keep = previous?.scrollTop ?? 0;
  if(app.sheet==='prefs'&&previous&&previous.querySelector('.picture-editor')?.getAttribute('data-owner')===targetScreen()?.id&&(previous.querySelector('button:active')||previous.contains(document.activeElement)&&document.activeElement?.matches('select,input,summary')))return;
  const previousTitle = previous?.getAttribute('aria-label'), previousRequest = previous?.dataset.pairRequest;
  if (app.sheet === 'add') {
    const st = app.state!, p = st.pairing, stage = pairStage(st), name = pairingAdapterName(p), who = p.name ?? '新 iPhone';
    const err = stage === 'failed' ? recentLinkError(st) : null;
    const cancellingStage=['waiting','code','finishing'].includes(stage);
    pairingConfirmation.placeControls();
    const status = {start: p.pairing_error ? '配对未完成' : '配对窗口已结束', waiting: '正在等待新 iPhone…', code: '核对两端的配对码', finishing: '正在保存配对…', connecting: `正在连接 ${who}…`, failed: `${who} 连接未完成`, connected: `${who} 已连接`}[stage];
    const busy = stage === 'waiting' || stage === 'finishing' || stage === 'connecting';
    modal('添加 iPhone', [h('div', {class: 'wait'}, busy ? h('i', {class: 'spinner', 'aria-hidden': 'true'}) : null, text(status, err ? `${err.title}。${err.hint}` : undefined), h('span', {class: 'mono'}, p.remaining_s != null ? mmss(p.remaining_s) : '')),
      app.pairCancelError?h('p',{class:'error',role:'alert'},app.pairCancelError):null,
      stage === 'code' ? pairingConfirmation.element : null,
      (stage === 'waiting' || (stage === 'start' && p.pairing_error)) ? nearbyPhones.view(p, {header: p.discovering && name ? `正在扫描 · 在列表里点新 iPhone，或在它的蓝牙里点“${name}”` : undefined}) : null,
      stage === 'connecting' || stage === 'connected' ? null : group(...[['在新 iPhone 上打开蓝牙和 Wi‑Fi', '保持靠近接收端'], [name ? `打开 设置 › 蓝牙，点“${name}”` : '蓝牙名称暂时未知', name ? '也可以在上面的列表里点这台 iPhone' : '可在附近列表中点这台 iPhone；展开日志查看名称读取错误'], ['确认配对码并点“配对”', '再点“使用 CarPlay”即完成']].map(([title, sub], i) => h('div', {class: 'row add-step'}, h('span', {class: 'add-number'}, String(i + 1)), text(title, sub)))),
      stage === 'failed' ? button('重新连接', async () => { await actions.cmd('connect', p.device ?? undefined); }) : null,
      stage === 'start' ? button('重新开始配对', () => void actions.cmd('openPairWindow')) : null,
      stage === 'connected' ? button('完成', actions.close) : null,
      foot(stage==='connecting'?'iPhone 弹出“使用 CarPlay？”时点“使用 CarPlay”。关闭面板不会停止连接。':cancellingStage?'配对窗口 3 分钟。两边数字一致再确认；取消配对不会删除已保存的手机。':'关闭面板不会删除已保存的手机或停止其连接。')],actions.close,app.pairCancelBusy?'正在取消…':cancellingStage?'取消':'关闭',app.pairCancelBusy);
  } else if (app.sheet === 'prefs') {
    const p = prefs(); if (!p) { actions.close(); return; }
    if(app.pref==='quality'){
      const s=app.state!,target=targetScreen()!;
      modal('画质',[pictureEditor.view({owner:target.id,prefs:p,window:target.window,spec:s.spec,limits:s.display_limits,maxRes:s.policy.max_res,maxFps:s.policy.max_fps,busy:app.prefsBusy,error:app.error,hevc:'remote',save:patch=>actions.prefs(patch,target.id),render},'phone-picture')]);
    }else{
    const opts: [string, string, string][] = app.pref === 'call' ? [['bt', '车机蓝牙（推荐）', '车自带的麦克风和扬声器'], ['web', '这块屏幕的麦克风', '需要 HTTPS · 切换后 iPhone 重新连接']] : [['left', '左侧', '控制栏靠左'], ['right', '右侧', '控制栏靠右']];
    modal({quality: '画质', call: '通话与 Siri', rail_side: '控制栏位置'}[app.pref], [group(...opts.map(([v, name, sub]) => h('button', {type: 'button', class: 'row', role: 'radio', 'aria-checked': String(p[app.pref] === v), onclick: async () => { if(await actions.prefs({[app.pref]: v}))actions.close(); }}, text(name, sub), h('span', {class: 'blue'}, p[app.pref] === v ? '✓' : ''))))]);
    }
  } else {pictureEditor.reset();clear($('overlay'));}
  const current = $('overlay').querySelector<HTMLElement>('.bottom-sheet');
  if (current) {
    current.dataset.pairRequest = `${app.state?.pairing.generation}/${app.state?.pairing.request}`;
    if (previousTitle === current.getAttribute('aria-label') && previousRequest === current.dataset.pairRequest) current.scrollTop = keep;
  }
}
const PAIR_TEXT: Record<PairStage, [string, string]> = {
  start: ['连接你的 iPhone', '通过蓝牙和 Wi‑Fi 连接 {receiver}，按提示确认配对和 CarPlay。'],
  waiting: ['在蓝牙里点“{name}”', '打开 设置 › 蓝牙，在“其他设备”里点“{name}”。出现配对码后，核对车机屏幕或回到这里。'],
  code: ['确认配对码', '数字一致就在 iPhone 弹窗里点“配对”，并在这里确认。'],
  finishing: ['正在完成配对', '两边都已确认，正在保存配对，完成后自动连接 CarPlay。'],
  connecting: ['正在连接 CarPlay', '配对已保存。iPhone 弹出“使用 CarPlay？”时点“使用 CarPlay”。'],
  failed: ['连接未完成', '配对已保存，可以重新连接。'],
  connected: ['已连接', '以后上车会自动连接，不需要再打开这个页面。'],
};
function renderPair(): void {
  if (!app.state) { clear($('content'), foot('正在读取设备状态…')); $('tabs').hidden = true; return; }
  const s = app.state, p = s.pairing, stage = pairStage(s), name = pairingAdapterName(p);
  const previous = $('content').querySelector<HTMLElement>('.pair-scroll'), keep = previous?.scrollTop ?? 0;
  const requestKey = `${p.generation}/${p.request}`, previousRequest = previous?.dataset.pairRequest;
  const err = stage === 'failed' ? recentLinkError(s) : null;
  const other = stage === 'start' && s.hub.phone ? s.phones?.find(x => x.current)?.name ?? '另一台 iPhone' : null;
  const bonded = stage === 'connecting' || stage === 'failed' || stage === 'connected';
  const steps: [string, string][] = [['已连上 CarPlay 热点', `${s.network.ssid ?? 'CarPlay 热点'} · 保持当前连接`],
    ['在 iPhone 上确认配对', bonded ? '配对已保存' : stage === 'code' ? '蓝牙弹窗显示与车机相同的 6 位数字' : stage === 'finishing' ? '正在保存配对…' : name ? `打开 设置 › 蓝牙，点“${name}”` : '蓝牙名称暂时未知，可在附近列表中选择 iPhone'],
    ['开始使用 CarPlay', stage === 'connected' ? 'CarPlay 已连接' : stage === 'connecting' ? '点“使用 CarPlay”，画面会出现在车机屏幕上' : '画面会出现在车机屏幕上']];
  const doneSteps = stage === 'connected' ? 3 : bonded ? 2 : 1;
  const [title, lead] = stage === 'waiting' && !name ? ['蓝牙名称暂时未知', '请展开日志与调试信息查看名称读取错误。也可以在下面的附近列表中选择要配对的 iPhone。'] : PAIR_TEXT[stage].map(x => x.replaceAll('{name}', name ?? '').replaceAll('{receiver}', receiverName(s)));
  const open = () => void actions.cmd('openPairWindow');
  const start = h('button',{type:'button',class:'primary',onclick:open},'开始配对');
  const wait = (text: string, clock = true) => h('div', {class: 'wait'}, h('i', {class: 'spinner', 'aria-hidden': 'true'}), text, clock && p.remaining_s != null ? h('span', {class: 'mono'}, mmss(p.remaining_s)) : null);
  const bottom = h('div', {class: 'pair-bottom'});
  if (stage === 'code') pairingConfirmation.placeControls(bottom);
  else bottom.append(...{
    start: [start, h('a', {class: 'secondary', href: '/phone'}, '稍后再说')],
    waiting: [wait('等待 iPhone…'), h('button', {type: 'button', class: 'secondary', disabled:app.pairCancelBusy, onclick: () => void cancelPairing(false)}, app.pairCancelBusy?'正在取消…':'取消')],
    finishing: [wait('正在保存配对…', false)],
    connecting: [wait('正在连接 CarPlay…', false), h('a', {class: 'secondary', href: '/phone'}, '打开 CowPlay 控制台')],
    failed: [h('button', {type: 'button', class: 'primary', onclick: () => void actions.cmd('connect', p.device ?? undefined)}, '重新连接'), h('a', {class: 'secondary', href: '/phone'}, '打开 CowPlay 控制台')],
    connected: [h('a', {class: 'primary', href: '/phone'}, '打开 CowPlay 控制台'), h('button',{type:'button',class:'secondary',onclick:open},'再添加一台 iPhone')],
  }[stage]);
  clear($('content'), h('div', {class: 'pair-scroll', 'data-pair-request': requestKey},
    h('header', {class: 'pair-brand'}, h('span', {class: 'app-icon'}, h('img',{src:brandIcon,alt:''})), h('h1', {}, title), h('p', {class: 'lead'}, err ? `${err.title}。${err.hint}` : other ? `${other} 正在投屏；开始配对会结束它的 CarPlay，配对都会保留。` : lead)),
    group(...steps.map(([label, sub], i) => {
      const complete = i < doneSteps;
      return h('div', {class: `row pair-step${complete ? ' complete' : i === doneSteps ? ' active' : ' pending'}`}, h('span', {class: 'step-num'}, complete ? '✓' : String(i + 1)), text(label, sub));
    })),
    app.error ? h('p', {class: 'error', role: 'alert'}, app.error) : null,
    app.pairCancelError&&!app.error?h('p',{class:'error',role:'alert'},app.pairCancelError):null,
    stage === 'code' ? pairingConfirmation.element : null,
    stage === 'waiting' || (stage === 'start' && p.pairing_error) ? nearbyPhones.view(p, {header: p.discovering ? '如果是用另一台手机打开的这页：在列表里点要配对的 iPhone' : undefined}) : null,
    stage === 'connected' ? h('section', {class: 'pair-done'}, text(s.hub.display === 'BROWSER_FRAME_RENDERED' ? '看车机屏幕就可以了' : s.hub.viewer ? 'CarPlay 已连接，等待车机显示画面' : '请在车机浏览器打开 CowPlay', '以后上车会自动连接，不需要再打开这个页面。可把控制台添加到主屏幕，方便重连和查看诊断。')) : null,
    foot(name ? `没有弹窗？打开 iPhone 设置 › 蓝牙，确认已开启；或者在 设置 › 通用 › CarPlay 里选择“${name}”。` : '没有弹窗？确认 iPhone 蓝牙已开启，并查看下方调试信息；设备名称读回后会自动更新。'),debug.update(s)),
    bottom);
  if (requestKey === previousRequest) $('content').querySelector<HTMLElement>('.pair-scroll')!.scrollTop = keep;
  $('tabs').hidden = true; document.documentElement.dataset.ready = 'true';
}
function authForm(message: string, setup = false): void {
  $('tabs').hidden = true;
  const input = h('input', {name: 'token', type: 'password', inputmode: 'numeric', pattern: '[0-9]{6}', maxlength: '6', autocomplete: 'off', 'aria-label': '访问令牌', required: true});
  const msg = h('p', {class: 'foot', role: 'status'}, message);
  const confirm = h('input', {type:'password',inputmode:'numeric',pattern:'[0-9]{6}',maxlength:'6',autocomplete:'new-password','aria-label':'确认访问码',required:setup});
  input.setAttribute('aria-label', setup?'设置访问码':'访问码');
  const submit = h('button', {type: 'submit', class: 'primary'}, setup?'保存并进入 CowPlay':'连接 CowPlay');
  const form = h('form', {class: 'auth'}, h('label',{},setup?'设置六位访问码':'访问码',input), ...(setup?[h('label',{},'再输入一次',confirm)]:[]), submit);
  form.onsubmit = async e => { e.preventDefault(); submit.disabled = true; try { if(setup) await setupAccess('phone',input.value,confirm.value,browserName()); else await admit('phone', {token: input.value, name: browserName()}); if(!(await authStatus()).authorized)throw Error('浏览器未保存授权，请允许此站点的 Cookie 后重试'); await authorized(); } catch (err) { msg.textContent = (err as Error).message; input.value = '';confirm.value='';if(setup){try{if(!(await authStatus()).setup_required)authForm((err as Error).message);}catch{}} } finally { submit.disabled = false; } };
  clear($('content'), h('h1', {}, 'CowPlay'), h('p', {class: 'lead'}, '连接、遥控与诊断'), msg, form, foot(setup?'此码保存在这台 CowPlay 上，供每个新设备首次访问。以后可通过受保护的管理入口修改或重置。':'请输入这台 CowPlay 首次设置的访问码。忘记时可由管理员修改或重置。'));
  document.documentElement.dataset.ready = 'true';
}
async function authorized(): Promise<void> { accessChanged();app.authorized = true; clearInterval(pollTimer); pollTimer = window.setInterval(() => void poll(), 2000); route(); await poll(); }
async function boot(): Promise<void> {
  if(new URLSearchParams(location.hash.slice(1)).has('t'))history.replaceState(null,'',location.pathname);
  try {
    const st = await authStatus();
    if (st.authorized) { await authorized(); return; }
    if(st.setup_required){authForm('首次使用：为这台 CowPlay 设置六位数字访问码。',true);return;}
    authForm(st.locked_s ? `错误次数太多，${Math.ceil(st.locked_s / 60)} 分钟后再试` : '输入六位访问码，此浏览器只需验证一次。');
  } catch (e) { authForm((e as Error).message); }
}
document.addEventListener('visibilitychange', () => { if(document.hidden){hideToken();render();}else void poll(); });
document.addEventListener('keydown', e => { if (e.key === 'Escape' && app.sheet && !document.querySelector('.action-confirmation[open]')) actions.close(); });
void boot();
