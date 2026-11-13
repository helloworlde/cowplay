// 02 · Session: control rail, picture region, debug info (YouTube "stats for nerds" style),
// stall recovery card, control centre and fullscreen handle.
import {$, clear, h} from '../shared/dom';
import {phoneModel, phoneWifi} from '../shared/api';
import {BUILD_ID} from '../build';
import {renderPictureSettings, type PictureActions} from './picture';
import {app} from './app';
import {browserName, even, renderManagementEntry, svg, switchRow} from './ui';

export interface SessionActions extends PictureActions {
  audioControls():HTMLElement;
  control(action:string):void;
  switchPhone(id: string): void;
  settings(tab?: string): void;
}

/** Picture region in CSS px: window minus rail, even sides. */
export function region(k: number): {w: number; h: number; rw: number; rh: number} {
  const w = window.innerWidth, hh = window.visualViewport?.height ?? window.innerHeight;
  const rail = app.fullscreen ? 0 : Math.max(64, Math.round(72 * k));
  return {w, h: hh, rw: even(w - rail), rh: even(hh)};
}
export function layout(k: number): void {
  const r = region(k);
  const el = $('region');
  el.style.width = `${r.rw}px`;
  el.style.height = `${r.rh}px`;
  fitCanvas();
}
export function fitCanvas(): void {
  const canvas = $<HTMLCanvasElement>('screen'), freeze = $<HTMLCanvasElement>('freeze'), el = canvas.parentElement!;
  const scale = Math.min(el.clientWidth / canvas.width, el.clientHeight / canvas.height);
  if (!(scale > 0)) return;
  for (const c of [canvas, freeze]) { c.style.width = `${Math.round(canvas.width * scale)}px`; c.style.height = `${Math.round(canvas.height * scale)}px`; }
}

// ---------- rail ----------
let lastClimateFeedback = '', climateNoticeUntil = 0;
export function renderRail(): void {
  const stalled = !!app.stall || (app.everRendered && !app.rendered);
  for (const id of ['r-siri','r-back','r-home']) $<HTMLButtonElement>(id).disabled = !app.channel || !app.rendered || app.stopped;
  $('r-sheet').setAttribute('aria-pressed', String(app.sheet));
  document.body.classList.toggle('stalled', stalled);
  $('h-dot').className = 'sdot' + (stalled ? ' warn' : '');
  const s = app.state, ultra = s?.hub.ultra;
  const status = $('climate-status');
  const feedback = ultra?.climate_control.feedback;
  const feedbackKey = feedback ? `${feedback.at_ms}/${feedback.sequence}` : '';
  if (feedback?.sequence && feedbackKey !== lastClimateFeedback) {
    lastClimateFeedback = feedbackKey;
    if (feedback.age_ms < 5000) {
      status.textContent = feedback.message;
      climateNoticeUntil = performance.now() + 4000;
    }
  }
  let visible = false;
  if (app.view === 'session' && app.channel && app.rendered && !app.stopped && !app.sheet
      && s?.hub.verified && s.hub.phone && ultra?.enabled && ultra.channels.some(Boolean)) {
    visible = performance.now() < climateNoticeUntil;
  }
  status.hidden = !visible;
}

// ---------- debug info ----------
export const history = {kbps: [] as number[], kb: [] as number[], buffer: [] as number[], intervalMs: 0};
export function pushHistory(kbps: number, kb: number, buffer: number, intervalMs: number): void {
  history.intervalMs = intervalMs;
  for (const [arr, v] of [[history.kbps, kbps], [history.kb, kb], [history.buffer, buffer]] as [number[], number][]) {
    arr.push(v);
    if (arr.length > 40) arr.shift();
  }
}
function spark(values: number[], color: string): HTMLElement {
  const max = Math.max(1e-6, ...values);
  const bars = Array.from({length: 40}, (_, i) => values[i - (40 - values.length)] ?? 0);
  return h('span', {class: 'spark', 'aria-hidden': 'true'}, ...bars.map(v => h('i', {style: `height:${Math.max(1, Math.round(v / max * 11))}px;background:${color}`})));
}
export interface Local {
  fps: number | null; decodeMs: number | null; decoded: number; dropped: number; frameW: number; frameH: number; hardware: boolean;
  codec: string | null; color: string | null; audio: {rate: number; channels: number; codec: string} | null; queueFrames: number | null;
  bytes: number; idle: boolean; arrivalAgeMs: number | null; paintAgeMs: number | null;
}
const dash = '—';
export function renderStats(l: Local): void {
  const box = $('stats');
  box.hidden = !app.state?.me.prefs.debug || app.view !== 'session';
  if (box.hidden) return;
  const receivedAge = app.statsAt ? performance.now()-app.statsAt : Infinity;
  const s = receivedAge<5000&&app.stats?.session?.id===app.state?.session?.id?app.stats:null, compact = document.body.classList.contains('compact');
  const w = window.innerWidth, hh = window.visualViewport?.height ?? window.innerHeight;
  const rg = $('region');
  const spec = s?.spec ?? app.state?.spec ?? null;
  const num = (v: number | null | undefined, d = 0, u = '') => v === null || v === undefined || !Number.isFinite(v) ? dash : `${v.toFixed(d)}${u}`;
  const systemAge = (s?.f50.sample_age_ms ?? Infinity)+receivedAge;
  const wifiAge = (s?.f50.wifi?.sample_age_ms ?? Infinity)+receivedAge;
  const f50 = systemAge<5000?s?.f50:null, wifi = wifiAge<15000?s?.f50.wifi:null;
  const ph = s?.phone;
  const kbps = history.kbps.at(-1), kb = history.kb.at(-1);
  const noAudio = app.state?.hub.phone ? '未收到音频流' : dash;
  const age = (ms: number | null) => ms === null ? dash : `${(ms/1000).toFixed(1)} s 前`;
  const fps = l.idle ? '无新帧' : num(l.fps, 0, ' fps（近 1 s）');
  const server = s?.server;
  const phone = s && wifi ? phoneWifi(s) : s ? 'Wi‑Fi 数据已过期 / 未采样' : dash;
  const rows: [number, string, string, HTMLElement?][] = [
    [0, '会话 / 屏幕', `${s?.session?.id ?? dash} / ${app.state?.me.id ?? dash}`],
    [0, '画布 / DPR', `${rg.clientWidth}×${rg.clientHeight} / ${devicePixelRatio.toFixed(2)}`],
    [1, '画面 / 声明', `${l.frameW ? `${l.frameW}×${l.frameH} · ${fps}` : dash} / ${spec ? `${spec.width}×${spec.height} · 上限 ${spec.fps} fps` : dash}`],
    [1, '本页绘制 / 丢弃', `${l.decoded} / ${l.dropped} 帧`],
    // Volume is not measurable here; show the audio format actually received, or say none arrived.
    [0, '音频', l.audio ? `${l.audio.codec} · ${(l.audio.rate / 1000).toFixed(l.audio.rate % 1000 ? 1 : 0)} kHz ${l.audio.channels >= 2 ? '立体声' : '单声道'}` : noAudio],
    [1, '解码器 / 回调', `${l.codec ?? dash} · ${l.hardware ? '硬件优先' : '软件'} / ${num(l.decodeMs, 1, ' ms（最近帧均值）')}`],
    [0, '颜色', l.color ?? dash],
    [1, '媒体接收速率', kbps===undefined?'采样中':`${kbps.toFixed(kbps<10?1:0)} Kbps · ${(history.intervalMs/1000).toFixed(1)} s 窗口${l.idle?'（无新帧）':''}`, spark(history.kbps, '#3EA6FF')],
    [0, '媒体流量', `本页累计 ${(l.bytes/1024).toFixed(1)} KiB · 本窗口 ${kb===undefined?'采样中':`${kb.toFixed(1)} KiB`}`, spark(history.kb, '#CCCCCC')],
    [1, '末次收帧 / 绘制', `${age(l.arrivalAgeMs)} / ${age(l.paintAgeMs)}`],
    [1, '解码队列', l.queueFrames === null ? '未配置解码器' : `${l.queueFrames} 帧${l.queueFrames===0?'（无积压）':''}`, spark(history.buffer, '#2BA640')],
    [1, '浏览器 ↔ 接收端', `RTT ${num(app.local.rttMs, 0, ' ms')} · 波动 ${num(app.local.jitterMs, 0, ' ms')}`],
    [0, '接收端收流', server&&f50?`${num(server.fps_in, 1, ' fps')} · ${num(server.bitrate_bps/1000, 1, ' Kbps')} / 队列丢弃 ${server.dropped} 帧`:dash],
    [0, '车机浏览器', `${browserName()} · ${w}x${hh} · ${document.visibilityState === 'visible' ? '前台' : '后台'}`],
    [1, '主机 CPU / 温度 / 内存', f50?`${num(f50.cpu, 0, '%')} / ${num(f50.temp, 0, '°C')} / ${num(f50.mem_free_mb, 0, ' MB')} 可用`:app.statsError||'等待主机新采样'],
    [0, '主机 Wi‑Fi / 上行', `${wifi?.channel ? `CH${wifi.channel}` : dash}${wifi?.width_mhz ? ` ${wifi.width_mhz}MHz` : ''} · ${wifi?.clients ?? dash} 台 / ${f50?.uplink ?? dash}`],
    [1, 'iPhone Wi‑Fi / iAP2', `${phone} / ${ph?.bt ?? dash}`],
    [0, 'iPhone 型号 / 系统', phoneModel(s)],
    [0, '页面 / 服务构建', `${BUILD_ID} / ${app.state?.build ?? dash}`],
    [0, '采样年龄', `接口 ${age(Number.isFinite(receivedAge)?receivedAge:null)} · 系统 ${age(Number.isFinite(systemAge)?systemAge:null)} · Wi‑Fi ${age(Number.isFinite(wifiAge)?wifiAge:null)}`],
  ];
  clear($('stats-rows'), ...rows.filter(r => !compact || r[0]).map(([, k, v, sp]) => h('div', {class: 'row',title:`${k}：${v}`}, h('span', {class: 'k'}, k), h('span', {class: 'v'}, v), sp ?? null)));
}

// ---------- stall card ----------
export function renderStall(retries: number, stallMs: number): void {
  const st = app.stall, linkStall = app.everRendered && !app.rendered && !app.stopped;
  const show = app.view === 'session' && (!!st || linkStall) && app.state?.policy.stall_card !== false;
  $('stall').hidden = !show;
  if (!show) return;
  const since = st?.since ?? 0, now = performance.now();
  const waited = st ? (now - since) / 1000 : 0;
  const exhausted = app.channelError || (st && st.attempts >= retries && now - st.lastTry >= stallMs);
  $('stall-title').textContent = exhausted ? '自动恢复未成功，请手动重连' : st ? '画面停住了，正在自动恢复' : app.channel ? '投屏通道已连接，正在恢复画面' : '连接中断了，正在自动恢复';
  $('stall-cause').textContent = st ? `原因：${st.cause} · 第 ${Math.max(1, st.attempts)} / ${retries} 次` : app.channel ? '等待新的画面，配对保留。' : '原因：车机与接收端的连接中断 · 正在重连';
  $('stall-time').textContent = st ? `${waited.toFixed(1)} 秒` : '';
  $('stall-bar').style.width = st ? `${Math.min(100, Math.round(((st.attempts - 1) + Math.min(1, (now - st.lastTry) / stallMs)) / retries * 100))}%` : '40%';
}

// ---------- control centre ----------
let phonesKey='',togglesKey='';
export function renderSheet(a: SessionActions): void {
  $('sheet').hidden = !app.sheet || app.view !== 'session';
  if ($('sheet').hidden) return;
  const s = app.state!, p = s.me.prefs;
  renderManagementEntry($('sheet-admin'), s.network.luci, s.network.platform);
  const phones = (s.phones ?? []).filter(x => x.online || x.current);
  const shown=(phones.length ? phones : (s.phones ?? []).slice(0, 2)).slice(0,3);
  const nextPhonesKey=JSON.stringify([s.me.id,shown.map(x=>[x.id,x.name,x.online,x.current])]);
  if(nextPhonesKey!==phonesKey){
  clear($('sheet-phones'), ...shown.map(x => h('button', {type: 'button', class: 'phone-card', role: 'radio', 'aria-checked': String(x.current), onclick: () => { if (!x.current) a.switchPhone(x.id); }},
    h('span', {class: `dot${x.online ? '' : ' off'}`}), h('span', {class: 't'}, h('b', {}, x.name), h('span', {}, x.current ? '当前' : x.online ? '在线 · 点按切换' : '离线')))));
    phonesKey=nextPhonesKey;
  }
  if(!$('sheet-input').childElementCount){
    for(const [action,title,name] of [['previous','上一首','上一首'],['play','播放/暂停','播放或暂停'],['next','下一首','下一首'],['select','确认','确认（相当于旋钮按下）']])$('sheet-input').append(h('button',{type:'button','data-action':action,'aria-label':name,onclick:()=>a.control(action)},svg(action,24),h('span',{},title)));
  }
  renderPictureSettings(a);
  const audioBox=$('sheet-audio');if(!audioBox.querySelector('button:active')&&!audioBox.contains(document.activeElement))audioBox.replaceChildren(a.audioControls());
  const nextTogglesKey=JSON.stringify([s.me.id,p.night,p.call,p.debug,app.prefsSaving]);
  if(nextTogglesKey!==togglesKey){
  clear($('sheet-toggles'),
    switchRow('trow', 'CarPlay 夜间外观', '深色地图与界面', p.night, () => a.setPrefs({night: !p.night})));
  clear($('sheet-call'),
    switchRow('trow', '使用这块屏幕的麦克风', '通话与 Siri 用屏幕采音 · 需要 HTTPS', p.call === 'web', () => a.setPrefs({call: p.call === 'web' ? 'bt' : 'web'})));
  clear($('sheet-debug'),
    switchRow('trow', '调试信息', '右上角显示画面、车机、接收端、iPhone 数据', p.debug, () => a.setPrefs({debug: !p.debug})));
    for(const button of $('sheet').querySelectorAll<HTMLButtonElement>('[role=switch]'))button.disabled=app.prefsSaving;
    togglesKey=nextTogglesKey;
  }
}
