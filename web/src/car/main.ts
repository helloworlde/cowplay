import {confirmAction,cancelConfirmation,preferenceChanges,preferencePrompt,commandPrompt,stateContext} from '../shared/confirmation';
import {AudioEngine} from '../audio';
import {VideoPlayer} from '../video';
import {InputController} from '../input';
import {parseWireMessage} from '../protocol';
import {BUILD_ID, PROTOCOL} from '../build';
import {$, h} from '../shared/dom';
import {ApiError, admit, api, authStatus, getDiag, getState, getStats, linkBusy, pairingActive, pairingCode, pairingFinishing, recentLinkError, runSegments, saveError, targetPhone, sendCmd, setPrefs as postPrefs, type ScreenPrefs, type State} from '../shared/api';
import {NearbyPhones, PairingConfirmation} from '../shared/pairing';
import {FrameProgress} from '../shared/frame-progress';
import {AudioControls} from './audio-controls';
import {MicUplink} from './mic';
import {app, onRender, phonesOf, render, type Tab} from './app';
import {fitConnect, renderConnect, scene, setupKeypad, type ConnectActions} from './connect';
import {fitCanvas, layout, pushHistory, region, renderRail, renderSheet, renderStall, renderStats, type Local} from './session';
import {renderPane, renderTabs, type SettingsActions} from './settings';
import {browserName, hydrateIcons, scale, toast} from './ui';
import {openControls, renderControlMetrics, resetPictureDraft} from './picture';
import {probeHevc} from '../shared/picture-editor';

hydrateIcons();
let authorization = 0;
type PendingSave = {authorization: number; client: string; prefs?: ScreenPrefs};
let pendingSave: PendingSave | null = null;
let viewEpoch=0,commandSequence=0;
const pendingCommands=new Map<string,object>();
let segmentsRequest:object|null=null;
const diagnosticContext=(state:State|null)=>JSON.stringify([state?.me.id,state?.viewer?.id,state?.current_phone,targetPhone(state)?.id,state?.session?.id,state?.stopped,state?.pairing.generation,state?.pairing.device]);
function clearDiagnostics():void{segmentsRequest=null;app.segments=null;app.segmentsRunning=false;app.diag=null;}

function authorizationChanged(): void {
  cancelConfirmation(); authorization++; commandSequence++; pendingCommands.clear(); pendingSave = null; resetPictureDraft(); clearDiagnostics();
  app.state = null;app.ticket=null;
  app.stats = null; app.statsAt = 0; app.statsError = '';
  app.prefsSaving = false; app.prefsError = '';
}
let k = scale().k;
let pairingAvailable = false;
const pairingDialog = h('dialog', {id: 'car-pairing-dialog', class: 'pairing-dialog'}) as HTMLDialogElement;
document.body.append(pairingDialog);
const pairingConfirmation = new PairingConfirmation(h('section', {id: 'car-pairing-confirmation'}), 'car-',
  body => api('/api/pairing/confirm', {body}), poll, undefined, () => app.authorized ? app.state?.me.id ?? '' : '');
pairingDialog.addEventListener('cancel', e => { e.preventDefault(); $<HTMLButtonElement>('car-reject').click(); });
const nearbyPhones = new NearbyPhones(poll, render, () => app.authorized ? app.state?.me.id ?? '' : '');
function syncPairing(): void {
  if (!app.authorized || !app.state || app.stopped || app.state.stopped) { pairingConfirmation.unavailable(); if (pairingDialog.open) pairingDialog.close(); return; }
  pairingConfirmation.update(app.state.pairing, !pairingAvailable);
  if (pairingConfirmation.visible && app.view === 'connect' && scene() === 'pair' && innerWidth >= 700) {
    if (pairingDialog.open) pairingDialog.close();
    $('c-body').querySelector(':scope > .code')?.replaceWith(pairingConfirmation.element);
  } else if (pairingConfirmation.visible) {
    if (pairingConfirmation.element.parentElement !== pairingDialog) pairingDialog.append(pairingConfirmation.element);
    if (!pairingDialog.open) { cancelConfirmation(); pairingDialog.showModal(); }
  } else if (pairingDialog.open) pairingDialog.close();
}

// ---------------- video channel (verified semantics kept from the previous page) ----------------
const canvas = $<HTMLCanvasElement>('screen'), freeze = $<HTMLCanvasElement>('freeze');
let socket: WebSocket | null = null, inputReady = false, retry = 0, retryTimer = 0, deadline = 0;
/** Reconnecting only because someone else lifted a stop: never take another screen's picture automatically. */
const ping = {sent: new Map<number, number>(), seq: 0, rtt: [] as number[], lost: [] as boolean[], latest: null as {seq: number; rtt_ms: number; at: number} | null};
let bytesIn = 0;
const send = (value: unknown): boolean => { if (app.channel && socket?.readyState === WebSocket.OPEN && socket.bufferedAmount < 4096) { socket.send(JSON.stringify(value)); return true; } return false; };
const audio = new AudioEngine(message => toast(`声音异常：${message.slice(0, 120)}`), (kind, timestamp_us) => send({type: 'audioprogress', kind, timestamp_us}));
const mixer=new AudioControls(audio,render);
let micStatus='等待 iPhone 请求语音输入';
const input = new InputController(canvas,message=>{
  if(!inputReady)return;
  // A release must reach the phone even when another pointer opened a panel.
  if(message.type==='touch'&&!message.contacts.some(c=>c.down)){send(message);return;}
  if(app.view==='session'&&!app.sheet)send(message);
});
const player = new VideoPlayer(canvas, message => {
  if (!message) return;
  input.setEnabled(false); inputReady = false; app.rendered = false; send({type: 'display', state: 'error'});
  $('cover-text').textContent = `画面暂时无法解码，正在自动恢复（${message.slice(0, 60)}）`; render();
}, () => {
  if (!app.rendered) {
    retry = 0; app.rendered = true; app.everRendered = true; inputReady = true; input.setEnabled(true); send({type: 'display', state: 'rendered'});
    $('cover').hidden = true; freeze.hidden = true; app.connectingSince = 0; app.addPhone = false;
    if (app.view === 'connect') setView('session');
    void keepAwake();
    render();
  }
  if (app.stall) endStall();
}, () => send({type: 'keyframe'}), () => { input.reset(); fitCanvas(); });
input.attach(); input.setEnabled(false);
const mic = new MicUplink(frame => { if (app.channel && socket?.readyState === WebSocket.OPEN && socket.bufferedAmount < 16384) socket.send(frame); }, text => {micStatus=text;toast(text);render();});

function snapshot(): void {
  if (!app.rendered || canvas.width < 2) return;
  freeze.width = canvas.width; freeze.height = canvas.height;
  try { freeze.getContext('2d')?.drawImage(canvas, 0, 0); freeze.hidden = false; } catch { freeze.hidden = true; }
}
/** The picture is gone (session ended, channel lost): keep the last frame greyed, stop input. */
function invalidate(note: string, stopAudio = true): void {
  snapshot(); input.setEnabled(false); inputReady = false; player.reset(); if (stopAudio) audio.stopAll();
  app.rendered = false; $('cover').hidden = !freeze.hidden; $('cover-text').textContent = note; render();
}
function closeSocket(): void {
  mic.stop(); clearTimeout(deadline); deadline = 0;
  const old = socket; socket = null; app.channel = false; ping.sent.clear(); ping.latest = null;
  ping.rtt.length = 0; ping.lost.length = 0;
  app.local.rttMs = null; app.local.jitterMs = null; app.local.lossPct = null;
  old?.close();
}
function windowInfo() { const r = region(k); return {w: r.w, h: r.h, dpr: devicePixelRatio, rw: r.rw, rh: r.rh}; }
function connectSocket(takeover = false): void {
  if (!app.authorized || app.stopped || !navigator.onLine || app.buildMismatch) return;
  app.channelError = null;
  closeSocket();
  const next = new WebSocket(`${location.protocol === 'https:' ? 'wss' : 'ws'}://${location.host}/ws`);
  socket = next; next.binaryType = 'arraybuffer';
  deadline = window.setTimeout(() => failed(next), 8000);
  next.onopen = () => { if (socket === next) next.send(JSON.stringify({build: BUILD_ID, protocol: PROTOCOL, takeover, window: windowInfo()})); };
  next.onmessage = event => {
    if (socket !== next) return;
    if (typeof event.data === 'string') { onText(next, event.data); return; }
    if (!app.channel) return;
    bytesIn += (event.data as ArrayBuffer).byteLength;
    const m = parseWireMessage(event.data);
    if (m?.kind === 'video-config') player.configure(m);
    else if (m?.kind === 'video-frame') player.pushFrame(m);
    else if (m?.kind === 'audio-config') audio.handleConfig(m);
    else if (m?.kind === 'audio-packet') audio.handlePacket(m);
    else if (m?.kind === 'audio-stopped') audio.stopStream(m.streamType);
  };
  next.onclose = () => failed(next); next.onerror = () => failed(next);
}
function onText(next: WebSocket, text: string): void {
  let m: any;
  try { m = JSON.parse(text); } catch { failed(next); return; }
  if (m.type === 'error') {
    clearTimeout(deadline);
    if (m.code === 'AUTH_REVOKED') {
      invalidate('正在重新核实连接授权');
      closeSocket();
      const owner = authorization;
      void verifyAuthorization(owner).then(() => {
        // Network failure is not revocation. Retain the existing bounded recovery;
        // a confirmed revocation changes authorization and cancels it below.
        if (owner === authorization && app.authorized) schedule();
      });
      return;
    }
    if (m.code === 'VIEWER_BUSY') { closeSocket(); app.takeover = {deadline: null}; render(); return; }
    if (m.code === 'VIEWER_REPLACED') { invalidate('画面已切到另一块屏幕'); closeSocket(); app.takeover = {deadline: null}; setView('connect'); return; }
    if (m.code === 'CLIENT_BUILD_MISMATCH') {
      closeSocket();
      // A stale bundle must never create a reload → keypad → reload loop.
      app.channelError = '页面版本已更新，请刷新页面';
      app.buildMismatch = true;
      invalidate(app.channelError); setView('connect'); render(); return;
    }
    failed(next); return;
  }
  if (m.type === 'hello') {
    clearTimeout(deadline); deadline = 0; app.channel = true; app.takeover = null;
    if (!app.connectingSince) app.connectingSince = Date.now();
    reportCaps(); void poll(); render(); return;
  }
  if (!app.channel) return;
  switch (m.type) {
    case 'pong': onPong(m.seq); return;
    case 'reload': location.reload(); return;
    case 'host-ui': location.hash = 'settings/pic'; return;
    case 'mic': if (m.state === 'start') void mic.start(m); else mic.stop(); return;
    case 'session-reset': invalidate('iPhone 会话已结束，正在等待重新连接'); app.connectingSince = Date.now(); return;
    case 'video-stop': invalidate('iPhone 暂停了画面', false); return;
    case 'video-restart': snapshot(); player.reset(); return;
  }
}
function failed(next: WebSocket): void {
  if (socket !== next) return;
  closeSocket();
  if (app.everRendered) invalidate('与接收端的连接中断，正在重连');
  schedule();
}
function schedule(): void {
  clearTimeout(retryTimer);
  if (!app.authorized || app.stopped || app.takeover) return;
  if (!navigator.onLine) { app.channelError = '网络暂不可用，恢复网络后会重新连接。'; render(); return; }
  if (retry >= 4) { app.channelError = '自动重连 4 次仍未成功，请检查网络后点“重新连接”。'; render(); return; }
  const s = Math.min(8, 2 ** retry); retry++;
  retryTimer = window.setTimeout(() => connectSocket(false), s * 1000);
  render();
}
window.addEventListener('online', () => { if (!socket) { retry = 0; schedule(); } });
window.addEventListener('offline', () => { clearTimeout(retryTimer); closeSocket(); if (app.everRendered) invalidate('网络暂不可用，恢复后会重新连接'); app.channelError = '网络暂不可用，恢复网络后会重新连接。'; render(); });

// ---------------- car ⟷ F50 round trip, jitter, loss ----------------
function onPong(seq?: number): void {
  const at = seq !== undefined ? ping.sent.get(seq) : [...ping.sent.values()][0];
  if (at === undefined) return;
  const received = performance.now(), sampleSeq = seq ?? [...ping.sent.keys()][0];
  ping.latest = {seq: sampleSeq!, rtt_ms: received - at, at: received};
  ping.sent.delete(sampleSeq);
  ping.rtt.push(received - at); if (ping.rtt.length > 30) ping.rtt.shift();
  ping.lost.push(false); if (ping.lost.length > 60) ping.lost.shift();
}
setInterval(() => {
  if (!app.channel) return;
  const now = performance.now();
  for (const [seq, at] of ping.sent) if (now - at > 3000) { ping.sent.delete(seq); ping.lost.push(true); if (ping.lost.length > 60) ping.lost.shift(); }
  if (ping.sent.size > 4) { failed(socket!); return; }
  if (send({type: 'ping'})) ping.sent.set(++ping.seq, now);
  const r = ping.rtt;
  const fresh = ping.latest !== null && now - ping.latest.at < 3000;
  app.local.rttMs = fresh && r.length ? r.reduce((a, b) => a + b, 0) / r.length : null;
  app.local.jitterMs = fresh && r.length > 1 ? r.slice(1).reduce((a, v, i) => a + Math.abs(v - r[i]), 0) / (r.length - 1) : null;
  app.local.lossPct = fresh && ping.lost.length >= 10 ? ping.lost.filter(Boolean).length * 100 / ping.lost.length : null;
}, 1000);

// ---------------- stall detection and three-step recovery ----------------
const frameProgress = new FrameProgress();
function endStall(): void {
  if (!app.stall) return;
  const ms = Math.round(performance.now() - app.stall.since);
  send({type: 'stall', ms}); app.stall = null; freeze.hidden = true; render();
}
setInterval(() => {
  const s = app.state;
  // After a disconnect there is nothing to recover: never call a phone back from here.
  if (!app.rendered || !s || app.stopped || s.stopped) { frameProgress.reset(); return; }
  const st = player.stats(), now = performance.now(), limit = s.policy.stall_ms, retries = s.policy.retries;
  const blocked = frameProgress.blockedSince(st.lastArrivalAt, st.lastPaintAt, now, limit);
  if (blocked === null) return;
  if (!app.stall) { app.stall = {since: blocked, attempts: 0, lastTry: 0, cause: '收到新画面，但绘制没有推进'}; snapshot(); }
  const stall = app.stall;
  if (now - stall.lastTry >= limit) {
    if (stall.attempts >= retries) { render(); return; }
    stall.attempts++; stall.lastTry = now;
    // Order: request a keyframe → rebuild the video stream → reconnect the iPhone.
    if (stall.attempts === 1) send({type: 'keyframe'});
    else if (stall.attempts === 2) void sendCmd('restartVideo').catch(() => send({type: 'keyframe'}));
    else void sendCmd('reconnect').catch(() => {});
  }
  render();
}, 500);

// ---------------- metrics report (server diagnosis uses them) ----------------
let lastBytes = 0,lastMetricsAt=performance.now();
setInterval(() => {
  const kb = (bytesIn - lastBytes) / 1024; lastBytes = bytesIn;
  const now=performance.now(),elapsed=Math.max(1,now-lastMetricsAt);lastMetricsAt=now;
  const st = player.stats();
  pushHistory(kb * 8192 / elapsed, kb, player.isConfigured ? player.decodeQueueSize : 0, elapsed);
  if (app.channel) send({type: 'metrics', report: {
    fps: st.fps ?? undefined, decode_ms: st.decodeMs === null ? undefined : +st.decodeMs.toFixed(2), rtt_ms: app.local.rttMs === null ? undefined : +app.local.rttMs.toFixed(1),
    ping: ping.latest ? {seq: ping.latest.seq, rtt_ms: ping.latest.rtt_ms, age_ms: Math.max(0, Math.round(performance.now() - ping.latest.at))} : undefined,
    jitter_ms: app.local.jitterMs === null ? undefined : +app.local.jitterMs.toFixed(1), loss_pct: app.local.lossPct ?? undefined,
    decoded: st.decoded, dropped: st.dropped, width: st.width || undefined, height: st.height || undefined, decoder: st.decoder, intervals: st.intervals.slice(-30).map(v => +v.toFixed(1)),
    codec: player.codecName ?? undefined, color: player.color ?? undefined,
    audio_rate: audio.info?.rate, audio_channels: audio.info?.channels, audio_codec: audio.info?.codec, audio_ms: audio.bufferedMs(),
    audio_underruns: audio.counters.underruns, audio_drops: audio.counters.drops, visible: document.visibilityState === 'visible',
  }});
  renderRail();
  renderStats(local());
  renderControlMetrics({width:st.width,height:st.height,fps:st.fps,codec:player.codecName});
  if (app.view === 'connect' && (app.choose?.deadline || app.takeover?.deadline)) render();
}, 1000);
function local(): Local {
  const st = player.stats();
  return {fps: st.fps, decodeMs: st.decodeMs, decoded: st.decoded, dropped: st.dropped, frameW: st.width, frameH: st.height, hardware: !/软件/.test(st.decoder),
    codec: player.codecName, color: player.color, audio: audio.info, queueFrames: player.isConfigured ? player.decodeQueueSize : null,
    bytes: bytesIn, idle: player.isConfigured && st.lastArrivalAt>0 && performance.now()-st.lastArrivalAt>1500,
    arrivalAgeMs: st.lastArrivalAt ? performance.now()-st.lastArrivalAt : null,
    paintAgeMs: st.lastPaintAt ? performance.now()-st.lastPaintAt : null};
}
function reportCaps(): void {
  send({type: 'caps', caps: {h264: typeof VideoDecoder !== 'undefined', fullscreen: !!document.fullscreenEnabled, width: innerWidth, height: innerHeight, dpr: devicePixelRatio, ua: navigator.userAgent.slice(0, 200), browser: browserName()}});
}

// ---------------- state polling and the connect flow ----------------
let pendingPoll: {authorization: number; epoch: number; promise: Promise<void>} | null = null, lastCall = 0, stateReadEpoch = 0;
async function poll(): Promise<void> {
  if (!app.authorized) return;
  if (pendingPoll?.authorization === authorization && pendingPoll.epoch === stateReadEpoch) return pendingPoll.promise;
  const request = {authorization, epoch:stateReadEpoch, promise: readState(authorization,stateReadEpoch)};
  pendingPoll = request;
  try { await request.promise; }
  finally { if (pendingPoll === request) pendingPoll = null; }
}
async function readState(generation: number, epoch: number): Promise<void> {
  try {
    const s = await getState();
    if (generation !== authorization || epoch !== stateReadEpoch || !app.authorized) return;
    const prev = app.state;
    if (pendingSave?.prefs && pendingSave.authorization === authorization && pendingSave.client === s.me.id) s.me.prefs = pendingSave.prefs;
    if(diagnosticContext(prev)!==diagnosticContext(s))clearDiagnostics();
    app.state = s;
    pairingAvailable = true;
    app.stateError=null;
    if (prev?.me.prefs.rail_side !== s.me.prefs.rail_side) { document.body.dataset.side = s.me.prefs.rail_side; relayout(); }
    // A stop lifted elsewhere (phone page, another screen) reopens this screen's video channel.
    if ((!prev || prev.stopped) && !s.stopped && app.stopped) { app.stopped = false; if (!socket) { retry = 0; connectSocket(false); } }
    leaveEndedSession(s);
    advance();
    render();
    // An authorization becomes usable only after its own first state arrives.
    // A later poll can finish this step if the first read temporarily failed.
    if (!prev) { route(); if (!socket) connectSocket(false); }
  } catch (e) {
    if (generation !== authorization || epoch !== stateReadEpoch) return;
    pairingAvailable = false; pairingConfirmation.unavailable();
    if (e instanceof ApiError && e.status === 401) {
      if (await verifyAuthorization(generation)) {
        app.stateError = '授权仍有效，暂时无法读取状态，正在重试。'; render();
      }
    }
    else {app.stateError=`状态更新失败：${(e as Error).message}。正在重试，下方为上次读取结果。`;render();}
  }
}
setInterval(() => void poll(), 2000);
setInterval(async () => {
  if (!app.authorized) return;
  const wantStats = (app.view === 'session' && (app.state?.me.prefs.debug||app.sheet)) || (app.view === 'settings' && (app.tab === 'devices' || app.tab === 'diag')) || scene() === 'takeover';
  if (wantStats) {
    const owner=authorization, session=app.state?.session?.id, started=performance.now();
    const current=()=>owner===authorization&&app.authorized&&session===app.state?.session?.id&&started>=app.statsAt&&performance.now()-started<5000;
    try {
      const stats=await getStats();
      if(!current()||(stats.session?.id ?? undefined)!==session)return;
      app.stats=stats;app.statsAt=started;app.statsError='';
    } catch { if(current())app.statsError='主机采样读取失败'; }
  }
}, 1000);
setInterval(async () => {
  if (!app.authorized) return;
  const owner=authorization,context=diagnosticContext(app.state);
  try { const result=await getDiag();if(owner!==authorization||!app.authorized||context!==diagnosticContext(app.state))return;app.diag=result;if(app.view==='settings')render(); } catch { /* keep last */ }
}, 3000);

/** The picture is gone for good (pairing started, someone disconnected, the iPhone session ended): show the home. */
let phoneGoneSince = 0;
function leaveEndedSession(s: State): void {
  phoneGoneSince = s.hub.phone ? 0 : phoneGoneSince || Date.now();
  if (app.view !== 'session' || app.rendered || app.stall) return;
  if (pairingActive(s.pairing) || s.stopped || (!app.sheet&&phoneGoneSince && Date.now() - phoneGoneSince > 6000)) { if (app.fullscreen) exitFull(); setView('connect'); }
}
/** Keep a first-use window open while the onboarding codes are on screen; back off after a failure. */
let onboardOpening = false;
function openOnboardWindow(s: State): void {
  const p = s.pairing;
  if (onboardOpening || pairingActive(p) || /^paired$/i.test(p.phase) || document.visibilityState !== 'visible') return;
  if (app.openError && Date.now() - app.openError.at < 10_000) return;
  onboardOpening = true;
  void sendCmd('openPairWindow').then(() => { app.openError = null; })
    .catch(e => { app.openError = {message: (e as Error).message, at: Date.now()}; })
    .finally(() => { onboardOpening = false; void poll(); });
}
/** Decide the next automatic step: pair window, choose list, calling the phone, retries. */
function advance(): void {
  // A manual invitation may already be running before its acknowledgement or the next link state.
  if(pendingCommands.size)return;
  const s = app.state;
  if (!s || !app.authorized) return;
  const phones = phonesOf(s), p = s.pairing;
  const sc = scene();
  if (sc === 'onboard') { void ensureTicket(); openOnboardWindow(s); }
  // The digits arrived (or the bond finished): the add scene has done its job.
  if (app.addPhone && (pairingCode(p) || pairingFinishing(p) || /^paired$/i.test(p.phase))) app.addPhone = false;
  if (s.hub.phone) { app.connectingSince = 0; if (p.generation > app.addFrom) app.addPhone = false; if (app.choose && !app.choose.manual) app.choose = null; }
  // An automatic “which one?” countdown must not outlive the situation that raised it.
  if (app.choose && !app.choose.manual && (pairingActive(p) || s.stopped || app.addPhone || linkBusy(s))) app.choose = null;
  // Never call an older phone while pairing, after a disconnect or while this screen adds a phone.
  if (!app.channel || app.stopped || s.stopped || app.addPhone || app.takeover || pairingActive(p) || p.pairing_error || recentLinkError(s) || !phones.length) return;
  if (s.hub.phone) return;
  if (!app.connectingSince) app.connectingSince = Date.now();
  const online = phones.filter(p => p.online);
  const busy = linkBusy(s);
  // Ask only when nothing is being called: a phone that has just paired is already being invited.
  if (!app.choose && !app.chooseDone && s.policy.ask_when_multi && online.length >= 2 && !busy && !/^paired$/i.test(p.phase)) {
    const pick = (online.find(p => p.preferred) ?? online.sort((a, b) => (b.last_ms ?? 0) - (a.last_ms ?? 0))[0]).id;
    app.choose = {manual: false, picked: pick, deadline: s.policy.ask_timeout ? Date.now() + s.policy.ask_timeout * 1000 : 0};
    return;
  }
  if (app.choose) return;
  const gap = Date.now() - app.connectingSince > 20_000 ? 5_000 : 15_000;
  if (!busy && Date.now() - lastCall > gap) { lastCall = Date.now(); void sendCmd('connect').catch(() => {}); }
}
setInterval(() => {
  const c = app.choose, st = app.state;
  if (c?.deadline && Date.now() >= c.deadline && c.picked && !(st && (pairingActive(st.pairing) || st.stopped))) { void pickPhone(c.picked,true); return; }
}, 250);
async function ensureTicket(): Promise<void> {
  if (app.ticket && Date.now() - app.ticket.at < 240_000) return;
  const owner=authorization,client=app.state?.me.id;
  try { const r = await api<{ticket: string}>('/api/auth/ticket', {body: {}});if(!app.authorized||owner!==authorization||client!==app.state?.me.id)return;app.ticket={value:r.ticket,at:Date.now()};render(); } catch { /* QR falls back to the plain URL */ }
}
async function pickPhone(id: string,automatic=false): Promise<void> {
  if(app.choose)app.choose.deadline=0;
  const cmd=app.state?.hub.phone?'switchPhone':'connect';
  const accepted=()=>{app.choose=null;app.chooseDone=true;app.stopped=false;app.connectingSince=Date.now();lastCall=Date.now();if(app.state)app.state.stopped=false;if(!socket){retry=0;connectSocket(false);}render();};
  if(automatic){accepted();const owner=authorization;void sendCmd(cmd,id).then(()=>{if(owner===authorization&&app.authorized)return poll();}).catch(()=>{});return;}
  await manualCommand(cmd,id,async current=>{accepted();await poll();if(!current())return;});
}
async function addPhone(): Promise<void> {
  await manualCommand('openPairWindow',undefined,()=>{
    app.addPhone=true;app.addFrom=app.state?.pairing.generation??0;app.choose=null;app.openError=null;app.stopped=false;
    if(app.state)app.state.stopped=false;
    if(!socket){retry=0;connectSocket(false);}
    if(location.hash)history.replaceState(null,'',location.pathname);
    route();void poll();
  });
}
async function takeoverNow(): Promise<void> {
 if(app.buildMismatch){location.reload();return;}
 const context=interactionContext(),current=()=>app.authorized&&interactionContext()===context;
 // VIEWER_BUSY also occurs between tabs sharing the same browser authorization.
 const other=!!app.takeover||!!app.state?.viewer&&app.state.viewer.id!==app.state.me.id;
 if(!await confirmAction({title:other?'在这块屏幕显示？':'重新连接这块屏幕？',description:other?'另一块屏幕将停止显示 CarPlay，画面和浏览器声音会切换到这里，iPhone 配对保留。':'将重新建立网页投屏连接，画面和浏览器声音会短暂中断，iPhone 配对保留。',confirm:other?'切换到这块屏幕':'重新连接屏幕',current})||!current())return;
 app.takeover=null;retry=0;connectSocket(other);render();
}

const connectActions: ConnectActions = {
  pick: pickPhone,
  openChoose() { const s = app.state; app.choose = {manual: true, picked: phonesOf(s).find(p => p.current || p.preferred)?.id ?? null, deadline: 0}; render(); },
  // Retry the phone that was being called (e.g. a just-paired one), not whichever is preferred.
  async retry() { if(app.buildMismatch){location.reload();return;} await manualCommand('connect',targetPhone(app.state)?.id,()=>{retry=0;app.channelError=null;if(!socket)connectSocket(false);app.connectingSince=Date.now();lastCall=Date.now();void poll();render();}); },
  takeoverNow,
  keep() {
    if (app.takeover) app.takeover = {deadline: null};
    if (app.choose?.manual) app.choose = null;
    render();
  },
  addPhone,
  cancelAdd() {
    app.addPhone = false; app.openError = null; app.connectingSince = Date.now();
    const p = app.state?.pairing;
    if (p && (pairingActive(p)||p.pairing_error)) void manualCommand('closePairWindow',undefined,()=>poll());
    render();
  },
  nearby: nearbyPhones,
  async token(code: string) {
    try {
      await admit('screen', {token: code, name: browserName()});
      // Verify the HttpOnly cookie on a second request, not just the token ACK.
      if (!(await authStatus()).authorized) return '浏览器未保存授权，请允许此站点的 Cookie 后重试';
      await authorized(); return null;
    }
    catch (e) { return e instanceof ApiError ? e.message : '暂时无法验证，请稍后再试'; }
  },
};

// ---------------- preferences ----------------
const interactionContext=()=>JSON.stringify([authorization,location.hash,app.view,app.sheet,stateContext(app.state)]);
async function permitCommand(cmd:string,id?:string):Promise<boolean>{
 const state=app.state;if(!state||!app.authorized)return false;
 const prompt=commandPrompt(cmd,state,id);if(!prompt)return true;
 const context=interactionContext(),current=()=>app.authorized&&interactionContext()===context;
 return await confirmAction({...prompt,current})&&current();
}
/** A delayed acknowledgement may not act on another page, authorization or newer command. */
async function manualCommand(cmd:string,id?:string,done?:(current:()=>boolean)=>void|Promise<void>):Promise<boolean>{
  const generation=cmd==='closePairWindow'?app.state?.pairing.generation:undefined;
  const key=`${cmd}/${id??''}${generation===undefined?'':`/${generation}`}`;
  if(pendingCommands.has(key)){toast('操作正在进行，请稍候');return false;}
  if(!await permitCommand(cmd,id))return false;
  if(pendingCommands.has(key))return false;
  const token={},owner=authorization,client=app.state?.me.id,sequence=++commandSequence;
  const context=()=>JSON.stringify([viewEpoch,location.hash,app.view,app.tab,app.sheet]);
  const source=context(),current=()=>app.authorized&&owner===authorization&&client===app.state?.me.id&&pendingCommands.get(key)===token&&sequence===commandSequence&&source===context();
  pendingCommands.set(key,token);
  try{
    // Server cleanup permits 8s quiesce + 3s media shutdown; keep the UI wait bounded beyond that budget.
    const result=await sendCmd(cmd,id,AbortSignal.timeout(15000),generation);
    if(!current())return false;
    if(cmd==='closePairWindow'&&result?.ok!==true)throw new Error('配对取消尚未得到确认，请核对状态后重试');
    if(result?.ok===false)throw new Error('操作未执行，请检查连接后重试');
    await done?.(current);
    return true;
  }catch(e){if(current()){const uncertain=e instanceof DOMException&&/Timeout|Abort/.test(e.name);toast(uncertain?'操作结果未确认：请求超时，正在刷新连接状态，请核对后重试':(e as Error).message);if(uncertain)void poll();}return false;}
  finally{if(pendingCommands.get(key)===token)pendingCommands.delete(key);}
}
async function setPrefs(patch: Partial<ScreenPrefs>): Promise<boolean> {
  let s = app.state;
  if (!s || !app.authorized) return false;
  if(app.prefsSaving){toast('设置正在保存，请稍候');return false;}
  if(Object.entries(patch).every(([key,value])=>s!.me.prefs[key as keyof ScreenPrefs]===value))return true;
  const access=authorization,owner=s.me.id,context=interactionContext(),base=JSON.stringify(s.me.prefs),changes=preferenceChanges(s.me.prefs,patch);
  const valid=()=>app.authorized&&authorization===access&&app.state?.me.id===owner&&interactionContext()===context&&JSON.stringify(app.state.me.prefs)===base;
  if(changes.length){
    if(!await confirmAction({...preferencePrompt(changes),current:valid})){render();return false;}
    if(!valid()){toast('设置或授权已变化，请重新调整');render();return false;}
    s=app.state!;
  }
  if(app.prefsSaving){toast('设置正在保存，请稍候');return false;}
  app.prefsSaving=true;app.prefsError='';
  const before = s.me.prefs;
  const saving: PendingSave = {authorization, client: s.me.id, prefs: {...before, ...patch}};
  pendingSave = saving;
  const current=()=>pendingSave===saving&&app.authorized&&authorization===saving.authorization&&app.state?.me.id===saving.client;
  const renegotiate = (['quality', 'resolution','fps','codec','call', 'ui_size', 'right_hand_drive'] as const).some(k => k in patch && patch[k] !== before[k]);
  s.me.prefs = saving.prefs!;
  if (patch.rail_side) { document.body.dataset.side = patch.rail_side; relayout(); }
  render();
  try {
    const r = await postPrefs(patch,undefined,AbortSignal.timeout(5000));
    if(!current())return false;
    app.state!.me.prefs=r.prefs;if(r.spec)app.state!.spec=r.spec;
    if (renegotiate && s.hub.phone) toast(patch.call ? '通话方式已切换，iPhone 将重新连接' : '已保存，正在重新协商画面；配对保留');
    return true;
  } catch (e) { if(!current())return false;app.state!.me.prefs=before;document.body.dataset.side=before.rail_side;app.prefsError=saveError(e);toast(app.prefsError); return false; }
  finally { if(pendingSave===saving){pendingSave=null;app.prefsSaving=false;render();} }
}
async function setLimits(patch:{max_res?:string;max_fps?:number}):Promise<void>{
  let s=app.state!;if(!s||!app.authorized)return;
  if(app.prefsSaving){toast('设置正在保存，请稍候');return;}
  const changes=Object.entries(patch).filter(([key,value])=>s!.policy[key as 'max_res'|'max_fps']!==value).map(([key,value])=>`${key==='max_res'?'设备分辨率上限':'设备帧率上限'}：${value==='2160p'?'4K':value}${key==='max_fps'?' 帧/秒':''}`);
  if(!changes.length){render();return;}
  const access=authorization,owner=s.me.id,context=interactionContext(),base=JSON.stringify([s.policy.max_res,s.policy.max_fps]);
  const valid=()=>app.authorized&&authorization===access&&app.state?.me.id===owner&&interactionContext()===context&&JSON.stringify([app.state.policy.max_res,app.state.policy.max_fps])===base;
  if(!await confirmAction({title:'修改所有屏幕的设备上限？',description:'此设置由所有屏幕共用。正在投屏的 CarPlay 会断开后重新连接，画面和声音将短暂中断，配对保留。',changes,confirm:'修改并重新连接',current:valid})){render();return;}
  if(!valid()){toast('设备上限或授权已变化，请重试');render();return;}
  s=app.state!;
  if(app.prefsSaving){toast('设置正在保存，请稍候');return;}
  const saving: PendingSave = {authorization, client: s.me.id};pendingSave=saving;
  const current=()=>pendingSave===saving&&app.authorized&&authorization===saving.authorization&&app.state?.me.id===saving.client;
  app.prefsSaving=true;app.prefsError='';render();
  try{
    const r=await api<{max_res:string;max_fps:number;spec:State['spec']}>('/api/policy',{body:patch,signal:AbortSignal.timeout(5000)});
    if(!current())return;
    Object.assign(app.state!.policy,{max_res:r.max_res,max_fps:r.max_fps});app.state!.spec=r.spec;
    toast('设备上限已保存，已请求重新连接 CarPlay');
  }catch(e){if(!current())return;app.prefsError=saveError(e);toast(app.prefsError);}
  finally{if(pendingSave===saving){pendingSave=null;app.prefsSaving=false;render();}}
}

// ---------------- views ----------------
function setView(v: typeof app.view): void {
  if(v!==app.view){viewEpoch++;cancelConfirmation();}
  if (v !== 'session') app.sheet = false;
  app.view = v; document.body.dataset.view = v; input.reset();
  if (v === 'session') relayout();
  if (v === 'settings' && !app.system) void api('/api/system').then(r => { app.system = r; render(); }).catch(() => {});
  render();
}
function route(): void {
  viewEpoch++;cancelConfirmation();
  const m = /^#settings(?:\/(\w+))?/.exec(location.hash);
  if (m) {
    if (!app.authorized) { history.replaceState(null, '', location.pathname); render(); return; }
    if (m[1] && ['pic', 'audio', 'display', 'phones', 'devices', 'diag', 'about'].includes(m[1])) app.tab = m[1] as Tab;
    if (app.tab === 'about') void ensureTicket();
    setView('settings');
  } else setView(app.rendered ? 'session' : 'connect');
}
window.addEventListener('hashchange', route);
const back = () => { app.addPhone = false; history.replaceState(null, '', location.pathname); route(); };
$('c-settings').addEventListener('click', () => { location.hash = 'settings'; });
$('s-back').addEventListener('click', back);

// session controls
$('r-phones').addEventListener('click', () => { location.hash='settings/phones'; });
for (const action of ['siri','back']) $('r-'+action).addEventListener('click', () => {
  if(control(action)){app.sheet=false;render();}
});
$('r-home').addEventListener('click', () => {
  if (!inputReady || app.view!=='session' || !send({type:'home'})) { toast('画面连接尚未就绪，请稍候'); return; }
  app.sheet=false; render();
});
$('stats-close').addEventListener('click', () => void setPrefs({debug: false}));
$('r-sheet').addEventListener('click', () => { viewEpoch++;if(app.sheet){app.sheet=false;render();}else openControls();resetStop(); });
$('sheet-done').addEventListener('click', () => { viewEpoch++;app.sheet = false; resetStop(); render(); });
$('sheet-all').addEventListener('click', e => { e.preventDefault(); location.hash = 'settings'; });
$('sheet-manage').addEventListener('click', e => { e.preventDefault(); location.hash = 'settings/phones'; });
const stopBtn = $('sheet-stop');
function resetStop(): void { stopBtn.classList.remove('armed'); stopBtn.textContent = '断开 CarPlay'; }
stopBtn.addEventListener('click', async () => {
  await manualCommand('disconnect',undefined,()=>{
    stateReadEpoch++;
    if(app.state)app.state.stopped=true;
    app.stopped=true;app.sheet=false;invalidate('CarPlay 已断开');closeSocket();app.everRendered=false;exitFull();setView('connect');
  });
});
async function reconnect(): Promise<void> {
  if (app.buildMismatch) { location.reload(); return; }
  if(!socket||!app.channel){await takeoverNow();return;}
  await manualCommand('reconnect',undefined,()=>{
    app.stall=null;app.connectingSince=Date.now();toast('已请求重新连接 iPhone，配对保留');void poll();
  });
}
$('r-re').addEventListener('click', reconnect);
$('stall-re').addEventListener('click', reconnect);
$('stall-video').addEventListener('click', () => { void sendCmd('restartVideo').then(() => toast('已重启视频，iPhone 连接保持')).catch(e => toast((e as Error).message)); });
$('r-full').addEventListener('click', enterFull);
$('handle').addEventListener('click', exitFull);
function enterFull(): void {
  app.sheet = false; app.fullscreen = true; document.body.classList.add('full'); $('handle').hidden = false;
  if (document.fullscreenEnabled && !document.fullscreenElement) void document.documentElement.requestFullscreen().catch(() => {});
  relayout(); render();
}
function exitFull(): void {
  app.fullscreen = false; document.body.classList.remove('full'); $('handle').hidden = true;
  if (document.fullscreenElement) void document.exitFullscreen().catch(() => {});
  relayout(); render();
}
document.addEventListener('fullscreenchange', () => { if (!document.fullscreenElement && app.fullscreen) exitFull(); });

// layout and window report (the server negotiates the picture from it)
let windowTimer = 0;
function relayout(): void {
  k = scale().k; layout(k);
  $('h-chev').replaceChildren(); $('h-chev').dataset.svg = document.body.dataset.side === 'right' ? 'chev-left' : 'chev-right'; hydrateIcons($('handle'));
  clearTimeout(windowTimer);
  windowTimer = window.setTimeout(() => send({type: 'window', window: windowInfo()}), 800);
  render();
}
window.addEventListener('resize', relayout); window.visualViewport?.addEventListener('resize', relayout);
const pictureResize=new ResizeObserver(fitCanvas);pictureResize.observe($('region'));pictureResize.observe(canvas.parentElement!);
document.addEventListener('pointerdown', () => { void audio.unlock().catch(() => {}); }, {once: false, passive: true});
let wake: any = null;
async function keepAwake(): Promise<void> { try { if (!wake && 'wakeLock' in navigator) { wake = await (navigator as any).wakeLock.request('screen'); wake.addEventListener('release', () => { wake = null; }); } } catch { /* optional */ } }
document.addEventListener('visibilitychange', () => { if (document.visibilityState === 'visible' && app.rendered) void keepAwake(); });

function control(action:string):boolean {
  if(!inputReady||app.view!=='session'||!app.rendered||app.stopped){toast('画面连接尚未就绪，请稍候');return false;}
  const media:Record<string,number>={play:3,next:4,previous:5};
  const message=action==='siri'?{type:'siri'}:media[action]?{type:'media',media:media[action]}:{type:'knob',knob:{select:action==='select',home:false,back:action==='back',x:0,y:0,wheel:0}};
  if(!send(message)){toast('控制未发送，请检查连接后重试');return false;}
  return true;
}
const settingsActions: SettingsActions = {
  setPrefs,
  audioControls:()=>mixer.view(micStatus),
  control,
  setLimits: p => void setLimits(p),
  async cmd(cmd,id){
    return manualCommand(cmd,id,async current=>{
      if(cmd==='connect'||cmd==='switchPhone'){
        app.stopped=false;app.connectingSince=Date.now();lastCall=Date.now();if(app.state)app.state.stopped=false;
        toast(cmd==='connect'?'已发送，正在呼叫这台 iPhone…':'已请求切换 iPhone，请等待连接完成');
        if(!socket){retry=0;connectSocket(false);}
      }
      await poll();if(!current())return;
      if(cmd==='connect')back();
    });
  },
  addPhone,
  async runTest(){
    if(app.segmentsRunning)return;
    const token={},owner=authorization,context=diagnosticContext(app.state);
    const current=()=>segmentsRequest===token&&owner===authorization&&app.authorized&&context===diagnosticContext(app.state);
    segmentsRequest=token;app.segmentsRunning=true;app.segments=null;render();
    try{
      const samples:number[]=[];
      for(let i=0;i<5;i++){const t=performance.now();try{await api('/api/ping');samples.push(performance.now()-t);}catch{break;}if(!current())return;}
      samples.sort((a,b)=>a-b);
      const result=await runSegments(samples.length?samples[Math.floor(samples.length/2)]:undefined);
      if(current())app.segments=result;
    }catch(e){if(current()&&app.view==='settings'&&app.tab==='diag')toast((e as Error).message);}
    finally{if(segmentsRequest===token){segmentsRequest=null;app.segmentsRunning=false;render();}}
  },
  back,
  local() { const r = region(k); return {rw: r.rw, rh: r.rh, hardware: !/软件/.test(player.stats().decoder), audio: audio.info, wake: !!wake}; },
};

onRender(() => {
  const focused = document.activeElement as HTMLElement | null;
  input.setEnabled(inputReady&&app.view==='session'&&!app.sheet);
  document.body.classList.toggle('controls-open',app.sheet&&app.view==='session');
  if (app.view === 'connect') renderConnect(connectActions);
  if (app.view === 'session') { renderSheet({setPrefs, audioControls:settingsActions.audioControls, control:settingsActions.control, setLimits:p=>void setLimits(p), switchPhone: id => void settingsActions.cmd('switchPhone', id), settings: t => { location.hash = t ? `settings/${t}` : 'settings'; }}); renderRail(); renderStats(local()); }
  const st=player.stats();renderControlMetrics({width:st.width,height:st.height,fps:st.fps,codec:player.codecName});
  if (app.view === 'settings') { renderTabs(); renderPane(settingsActions); }
  syncPairing();
  if (app.view === 'connect') fitConnect();
  if (focused?.isConnected && focused !== document.body) focused.focus({preventScroll: true});
  const s = app.state;
  if (s) renderStall(s.policy.retries, s.policy.stall_ms);
});

// ---------------- authorization and start ----------------
let authorizationCheck: {owner:number; promise:Promise<boolean>} | null = null;
function verifyAuthorization(owner:number):Promise<boolean> {
  if (authorizationCheck?.owner === owner) return authorizationCheck.promise;
  const request = {owner,promise:checkAuthorization(owner)};
  authorizationCheck = request;
  void request.promise.finally(() => { if (authorizationCheck === request) authorizationCheck = null; });
  return request.promise;
}
async function checkAuthorization(owner:number):Promise<boolean> {
  try {
    const status = await authStatus();
    if (owner !== authorization || !app.authorized) return false;
    if (status.authorized) return true;
    invalidate('这块屏幕的授权已失效'); closeSocket();
    app.authorized = false; authorizationChanged(); app.everRendered = false;
    clearTimeout(retryTimer); setView('connect'); render();
  } catch {
    if (owner === authorization) { app.stateError = '暂时无法核实授权，请检查网络后重试。'; render(); }
  }
  return false;
}
async function authorized(): Promise<void> {
  authorizationChanged();
  app.authorized = true;
  await poll();
}
async function boot(): Promise<void> {
  void probeHevc().then(supported=>{app.hevcSupport=supported;render();});
  setupKeypad(connectActions);
  relayout();
  if (!isSecureContext) { $('c-title').textContent = '需要可信的 HTTPS 地址'; $('c-lead').textContent = '请用 接收端证书匹配的 https:// 地址打开，不要绕过证书警告。'; return; }
  try {
    const st = await authStatus();
    if (st.authorized) { await authorized(); return; }
    if (!st.required) {
      await admit('screen', {name: browserName()});
      if ((await authStatus()).authorized) { await authorized(); return; }
      $('token-msg').textContent = '浏览器未保存授权，请允许此站点的 Cookie 后重试';
      $('token-msg').className = 'err';
    }
    if (st.locked_s) { $('token-msg').textContent = `错误次数太多，${Math.ceil(st.locked_s / 60)} 分钟后再试`; $('token-msg').className = 'err'; }
  } catch { $('token-msg').textContent = '暂时连不上 CowPlay，稍后自动重试'; setTimeout(() => void boot(), 5000); }
  render();
}
window.addEventListener('pagehide', () => { closeSocket(); input.detach(); });
window.addEventListener('pageshow', () => input.attach());
void boot();
