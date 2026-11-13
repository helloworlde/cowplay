import {receiverName, receiverPlatform} from '../shared/receiver';
// 03 · Settings (parked): picture, calls, display, iPhone, devices, diagnosis, about.
import {$, clear, h} from '../shared/dom';
import {qrSvg} from '../shared/qr';
import {diagnosticMeasurements, diagnosticReadings} from '../shared/diagnostics';
import {linkBusy, phoneModel, phoneWifi, type Phone, type Quality} from '../shared/api';
import {app, type Tab} from './app';
import {ago, browserName, minutes, renderManagementEntry, svg, switchRow} from './ui';
import {openControls, pictureControls, type PictureActions} from './picture';
import {createTeslaSettings} from './tesla';
const tesla = createTeslaSettings();
export const leaveSettings = (clear = false) => tesla.hide(clear);

export interface SettingsActions extends PictureActions {
  cmd(cmd: string, id?: string): Promise<boolean>;
  addPhone(): void;
  runTest(): void;
  back(): void;
  audioControls():HTMLElement;
  control(action:string):void;
  local(): {rw: number; rh: number; hardware: boolean; audio: {rate: number; channels: number} | null; wake: boolean};
}
const TITLES: Record<Tab, [string, string]> = {pic: ['画面', '清晰度与流畅度'], audio: ['声音与通话', '只影响这块屏幕'], display: ['显示', '只影响这块屏幕'], phones: ['iPhone', '所有屏幕共用'], tesla: ['Tesla 车辆', '配对、读数与空调控制'], devices: ['设备信息', '只读'], diag: ['诊断', '实时数据'], about: ['关于', '']};
const QN: Record<Quality, string> = {auto: '自动', smooth: '流畅', sharp: '清晰', custom:'自定义'};
let shownTab: Tab | null = null, tabsHeight = 0;
let tabsOwner = '', paneContext = '';

const foot = (t: string) => h('p', {class: 'foot'}, t);
const group = (...c: (HTMLElement | null)[]) => h('div', {class: 'group'}, ...c);

export function renderTabs(): void {
  const s = app.state, p = s?.me.prefs;
  renderManagementEntry($('s-admin'), s?.network.management ?? s?.network.luci ?? null, s?.network.platform);
  const tabs = $('s-tabs'), scroll = tabs.scrollTop;
  const owner = s?.me.id ?? '';
  if (tabsOwner === owner && tabs.querySelector('button:active,a:active')) return;
  tabsOwner = owner;
  const online = (s?.phones ?? []).filter(x => x.online).length;
  const defs: [Tab, string, string, string, string, string?][] = [
    ['pic', '画面', 'picture', 'var(--blue)', p ? QN[p.quality] : ''],
    ['audio', '声音与通话', 'speaker', 'var(--green)', p ? (p.call === 'bt' ? '不使用屏幕麦克风' : '屏幕麦克风') : ''],
    ['display', '显示', 'layout', '#5E5CE6', p ? (p.rail_side === 'left' ? '左侧' : '右侧') : ''],
    ['phones', 'iPhone', 'phone', 'var(--blue)', s?.phones ? `${online} 台在线` : ''],
    ['tesla', 'Tesla 车辆', 'vehicle', 'var(--red)', s?.vehicle?.connected ? '已连接' : '配对与控制'],
    ['devices', '设备信息', 'info', 'var(--idle)', ''],
    ['diag', '诊断', 'pulse', 'var(--orange)', '实时数据'],
    ['about', '关于', 'more', 'var(--idle)', s?.version ?? ''],
  ];
  clear(tabs, ...defs.map(([id, label, glyph, color, val, cls]) => h('button', {type: 'button', id: `settings-tab-${id}`, class: 's-tab', 'aria-current': app.tab === id ? 'page' : 'false', onclick: () => { location.hash = `settings/${id}`; }},
    h('span', {class: 's-ico', style: `background:${color}`}, svg(glyph, 18)), h('span', {class: 'lbl'}, label), h('span', {class: `val ${cls ?? ''}`}, val))));
  tabs.scrollTop = scroll;
  // Fade the bottom edge while more groups sit below the fold (compact car screens).
  const more = () => { tabs.classList.toggle('more', tabs.scrollHeight - tabs.scrollTop - tabs.clientHeight > 4); };
  if (!tabs.dataset.fade) { tabs.dataset.fade = '1'; tabs.addEventListener('scroll', more, {passive: true}); }
  more();
  if (shownTab !== app.tab || tabsHeight !== tabs.clientHeight) {
    const current = tabs.querySelector<HTMLElement>('[aria-current=page]');
    if (current) {
      const r = current.getBoundingClientRect(), box = tabs.getBoundingClientRect();
      if(window.matchMedia('(max-width:680px)').matches)tabs.scrollLeft+=Math.min(0,r.left-box.left)+Math.max(0,r.right-box.right);
      else tabs.scrollTop += Math.min(0, r.top - box.top) + Math.max(0, r.bottom - box.bottom);
    }
  }
  shownTab = app.tab; tabsHeight = tabs.clientHeight;
}

export function renderPane(a: SettingsActions): void {
  const s = app.state;
  const pane = $('s-pane');
  if (app.tab !== 'tesla' || !s) tesla.hide();
  const context = `${s?.me.id ?? ''}/${app.tab}${app.tab === 'tesla' ? location.hash : ''}`;
  // Polling must not replace a pressed control before its native click is delivered.
  if (paneContext === context && (pane.querySelector('button:active') ||
      (pane.contains(document.activeElement) && document.activeElement?.matches('select, summary, input')))) return;
  paneContext = context;
  const keep = pane.scrollTop;
  const [title, scope] = TITLES[app.tab];
  const head = h('div', {class: 's-pane-head'}, h('h1', {}, title), h('span', {class: 'scope'}, scope));
  if (!s) { clear(pane, head); return; }
  const p = s.me.prefs, loc = a.local();
  const body: (HTMLElement | null)[] = [];
  switch (app.tab) {
    case 'tesla': {
      const vehicle = tesla.show(s.me.id);
      if (pane.contains(vehicle)) return;
      body.push(vehicle);
      break;
    }
    case 'pic':
      if(app.everRendered)body.push(h('button',{type:'button',class:'picture-launch',onclick:()=>{a.back();openControls();}},'在控制面板中边看边调 ›'));
      body.push(pictureControls(a,'settings-picture'));
      break;
    case 'audio':
      body.push(group(switchRow('row', '使用这块屏幕的麦克风', '通话与 Siri 用屏幕采音 · 需要 HTTPS', p.call === 'web', () => a.setPrefs({call: p.call === 'web' ? 'bt' : 'web'}))));
      body.push(foot('音乐始终从这块屏幕播放。关闭时由 iPhone 选择通话输入；切换后 iPhone 会重新连接，配对保留。'));
      body.push(a.audioControls());
      if (loc.audio) body.push(group(h('div', {class: 'row'}, h('span', {class: 't'}, '音乐与导航声音'), h('span', {class: 'val'}, `这块屏幕 · ${Math.round(loc.audio.rate / 1000)} kHz ${loc.audio.channels >= 2 ? '立体声' : '单声道'}`))));
      break;
    case 'display':
      body.push(h('div', {class: 'g-label'}, '控制栏位置（放在靠近驾驶员的一侧）'));
      body.push(h('div', {class: 'side-cards', role: 'radiogroup', 'aria-label': '控制栏位置'}, ...(['left', 'right'] as const).map(side => h('button', {type: 'button', class: 'side-card', role: 'radio', 'aria-checked': String(p.rail_side === side), onclick: () => a.setPrefs({rail_side: side})},
        h('span', {class: `mini ${side}`}, h('i'), h('b')), h('span', {class: 't'}, h('b', {}, side === 'left' ? '左侧' : '右侧'), h('small', {}, side === 'left' ? '控制栏靠左' : '控制栏靠右'))))));
      body.push(group(
        switchRow('row', 'CarPlay 夜间外观', null, p.night, () => a.setPrefs({night: !p.night})),
        switchRow('row', '投屏时显示调试信息', '右上角半透明小窗；也可在“控制”中开关', p.debug, () => a.setPrefs({debug: !p.debug}))));
      break;
    case 'phones': {
      const list = s.phones ?? [];
      const live = s.hub.phone || linkBusy(s);
      const add = h('button', {type: 'button', class: 'add-row', onclick: () => a.addPhone()}, '＋ 添加 iPhone');
      body.push(group(...list.map(x => phoneRow(x, a, live)), add));
      if (list.length) {
        const preferred = list.find(x => x.preferred)?.name;
        const rule = s.policy.ask_when_multi ? `多台在线时在车机上询问${s.policy.ask_timeout ? `，${s.policy.ask_timeout} 秒后自动连接` : ''}` : '不询问，直接连接';
        body.push(group(h('div', {class: 'row'}, h('span', {class: 't'}, h('span', {}, '自动连接'), h('small', {}, `${preferred ? `先连首选“${preferred}”` : '按最近使用顺序'} · ${rule}`)))));
      }
      body.push(foot(s.phones_error ? `暂时读不到 iPhone 列表：${s.phones_error}` : live ? '可以直接切换到其他已保存的 iPhone，无需先断开；请保持目标手机蓝牙和 Wi-Fi 开启。正在使用的 iPhone 不能忘记。' : '上车先连“首选”，不在就按最近使用顺序试。点“连接”会呼叫那台 iPhone。'));
      break;
    }
    case 'devices': body.push(...devices(a)); break;
    case 'diag': body.push(...diagnosis(a)); break;
    case 'about': {
      const noticesOpen = pane.querySelector<HTMLDetailsElement>('.about-notices')?.open ?? false;
      const rows: [string, string, boolean][] = [['版本', s.version, false], ['构建', s.build, true], ['访问地址', s.network.host, true], ['这块屏幕', `${browserName()} · 已授权`, false], ['连接方式', '无线 CarPlay', false], ['服务运行', minutes(s.uptime_s), false]];
      const qr = (text: string | null, t: string, sub: string, aria: string) => {
        const box = h('div', {class: 'qr-box', role: 'img', 'aria-label': aria});
        if (text) { try { box.append(qrSvg(text, 200, '#000000', '#FFFFFF')); } catch { box.textContent = '二维码'; } } else box.textContent = '—';
        return h('div', {class: 'qr-small'}, box, h('span', {class: 't'}, h('b', {}, t), h('span', {class: 'sub-text'}, sub)));
      };
      body.push(h('div', {class: 'about-grid'},
        group(...rows.map(([k, v, mono]) => h('div', {class: 'row'}, h('span', {class: 't'}, k), h('span', {class: `val${mono ? ' mono' : ''}`, style: mono ? 'font-size:max(12px,calc(13px*var(--k)))' : ''}, v)))),
        h('div', {class: 'qr-col'},
          qr(s.network.management ?? s.network.luci, s.network.management || s.network.platform === 'linux' ? 'CowPlay 管理' : '管理页（OpenWrt LuCI）', s.network.management || s.network.platform === 'linux' ? '画面上限、证书、访问码、Tesla' : 'Wi‑Fi、证书、DDNS、访问码', '管理页二维码'),
          qr(`${location.origin}/phone`, '手机控制台', '遥控、诊断、日志', '手机控制台二维码'))));
      body.push(h('a', {id: 'settings-reset-page', class: 'row', href: '/car/reset'}, '清理页面缓存并重新打开'));
      body.push(h('details', {class: 'about-notices', open: noticesOpen},
        h('summary', {}, '开源许可'),
        h('p', {}, 'Based on LIVI by Lasse Heitgres (', h('a', {href: 'https://github.com/f-io/LIVI', target: '_blank', rel: 'noopener noreferrer'}, 'https://github.com/f-io/LIVI'), ')')));
    }
  }
  clear(pane, head, ...body);
  pane.scrollTop = keep;
}

/** Allow an explicit switch to any saved phone; Bluetooth offline is not a prohibition. */
function phoneRow(x: Phone, a: SettingsActions, live: boolean): HTMLElement {
  const sub = (x.current ? '正在投屏' : x.online ? '在线' : `离线${x.last_ms ? ` · ${ago(x.last_ms)}` : ''}`) + (x.preferred ? ' · 首选' : '');
  const forget = h('button', {type: 'button', class:'forget', disabled:x.current, onclick:()=>{if(!x.current)void a.cmd('forgetPhone',x.id);}}, x.current?'投屏中':'忘记');
  return h('div', {class: 'row'}, h('span', {class: `pdot${x.online ? ' on' : ''}`}), h('span', {class: 't'}, h('span', {}, x.name), h('small', {}, sub)),
    !x.current && live ? h('button', {type: 'button', class: 'tint', onclick: () => void a.cmd('switchPhone', x.id)}, '切到这台') : null,
    !live ? h('button', {type: 'button', class: 'tint', onclick: () => void a.cmd('connect', x.id)}, '连接') : null,
    !x.preferred ? h('button', {type: 'button', class: 'gray', onclick: () => void a.cmd('setPreferred', x.id)}, '设为首选') : null,
    forget);
}

function devices(a: SettingsActions): HTMLElement[] {
  const s = app.state!, st = app.stats, loc = a.local();
  const num = (v: number | null | undefined, u = '') => v === null || v === undefined ? '—' : `${Math.round(v)}${u}`;
  const cur = (s.phones ?? []).find(x => x.current) ?? (s.phones ?? []).find(x => x.preferred);
  const me = s.screens.find(x => x.id === s.me.id);
  const sys = app.system;
  const phoneState = s.hub.phone ? 'ok' : 'idle';
  const devs = {
    car: ['车', '这块屏幕', `${browserName()} · 已授权`, s.me.viewing ? '当前投屏屏幕' : '未投屏', s.me.viewing ? 'ok' : 'idle', [
      ['浏览器', browserName()], ['窗口', `${window.innerWidth}×${window.innerHeight} · DPR ${devicePixelRatio}`, 1], ['画面区', `${loc.rw}×${loc.rh}（偶数对齐）`, 1],
      ['解码器', st?.car?.decoder ?? '—'], ['地址', me?.ip || '—', 1],
      ['到接收端', `RTT ${num(app.local.rttMs, ' ms')} · 抖动 ${num(app.local.jitterMs, ' ms')}`, 1],
      ['页面', `${document.visibilityState === 'visible' ? '前台' : '后台'} · ${loc.wake ? '屏幕常亮' : '未保持常亮'}`], ['安全连接', `HTTPS · ${s.network.cert_days !== null ? (s.network.cert_days >= 0 ? '证书有效' : '证书过期') : '证书未知'}`]]],
    f50: ['主机', `${receiverName(s)} · ${sys?.model ?? receiverPlatform(s)}`, `${sys?.os ?? sys?.openwrt ?? receiverPlatform(s)} · 运行 ${minutes(s.uptime_s)}`, '服务接口可用', 'ok', [
      ['CPU · 温度', `${num(st?.f50.cpu, '%')} · ${num(st?.f50.temp, '°C')}`, 1], ['内存可用', `${num(st?.f50.mem_free_mb)} / ${num(st?.f50.mem_total_mb)} MB`, 1],
      ['Wi-Fi 热点', `${s.network.ssid ?? '—'} · ${st?.f50.wifi?.channel ?? s.network.channel ? `CH${st?.f50.wifi?.channel ?? s.network.channel}` : '—'}`], ['客户端', st?.f50.wifi?.clients != null ? `${st.f50.wifi.clients} 台` : '—'],
      ['上行', st?.f50.uplink ?? '—'], ['蓝牙', sys?.bluez ?? '—'],
      ['证书', s.network.cert_days !== null ? `剩 ${s.network.cert_days} 天${s.network.cert_source === 'acme' ? ' · ACME' : ''}` : '—']]],
    phone: ['iP', cur?.name ?? 'iPhone', s.hub.phone ? `CarPlay 会话 · ${s.session ? minutes((s.now_ms - s.session.started_ms) / 1000) : ''}` : '未连接', s.hub.phone ? '会话已建立' : '会话未建立', phoneState, [
      ['型号 · 系统', phoneModel(st)], ['地址', st?.phone.ip ?? '—', 1], ['Wi‑Fi 信号', phoneWifi(st), 1],
      ['蓝牙', cur?.bt ?? '—', 1], ['画面', s.spec && s.hub.phone ? `${s.spec.width}×${s.spec.height} @ ${s.spec.fps}` : '—', 1],
      ['码率', st && s.hub.video ? `${(st.server.bitrate_bps / 1e6).toFixed(1)} Mbps` : '—', 1], ['通话', (s.viewer?.prefs.call ?? s.me.prefs.call) === 'web' ? '车机屏幕麦克风' : '不使用屏幕麦克风'], ['首选', cur?.preferred ? '是' : '否']]],
  } as const;
  const tabDot = {car: s.me.viewing ? '#30D158' : '#636366', f50: '#30D158', phone: s.hub.phone ? '#30D158' : '#636366'};
  const dv = devs[app.devTab];
  const tabs = h('div', {class: 'segs wide', role: 'tablist', 'aria-label': '设备'}, ...(['car', 'f50', 'phone'] as const).map(t => h('button', {type: 'button', role: 'tab', 'aria-selected': String(app.devTab === t), 'aria-checked': String(app.devTab === t), onclick: () => { app.devTab = t; renderPane(a); }},
    h('i', {style: `background:${tabDot[t]}`}), {car: '车机屏幕', f50: receiverPlatform(s), phone: 'iPhone'}[t])));
  const card = h('section', {class: 'group'},
    h('div', {class: 'dev-head'}, h('span', {class: 'dev-badge'}, dv[0]), h('span', {class: 't', style: 'flex:1;display:flex;flex-direction:column;gap:2px;min-width:0'}, h('span', {class: 'dev-title'}, dv[1]), h('span', {class: 'sub-text'}, dv[2])), h('span', {class: `pill ${dv[4] === 'ok' ? '' : dv[4]}`}, `● ${dv[3]}`)),
    h('div', {class: 'kv-grid'}, ...dv[5].map((r: readonly (string | number)[]) => h('div', {class: 'kv'}, h('span', {class: 'sub-text'}, String(r[0])), h('span', {class: `v${r[2] ? ' mono' : ''}${r[3] ? ` ${r[3]}` : ''}`, title: String(r[1])}, String(r[1]))))));
  return [tabs, card, foot(`诊断码 ${s.diag_code}，用于对照设备日志。`)];
}

function diagnosis(a: SettingsActions): HTMLElement[] {
  const d = app.diag;
  if (!d) return [foot('正在读取诊断…')];
  const checks = diagnosticMeasurements(d.checks);
  const events = h('section', {class: 'group'}, h('div', {class: 'card-head'}, h('span', {}, '事件记录')),
    ...(d.events.length ? d.events.slice(0, 4).map(e => h('div', {class: `ev ${e.level}`}, h('i'), h('span', {class: 't'}, h('span', {}, e.title), e.cause ? h('span', {class: 'sub-text'}, e.cause) : null), h('time', {}, new Date(e.ts_ms).toTimeString().slice(0, 5)))) : [h('div', {class: 'ev'}, h('span', {class: 't'}, h('span', {class: 'sub-text'}, '暂无事件记录')))]));
  const seg = app.segments;
  const tests = h('section', {class: 'group'}, h('div', {class: 'card-head'}, h('span', {}, '连接检查'), h('button', {type: 'button', class: 'tint', disabled: app.segmentsRunning, onclick: () => a.runTest()}, app.segmentsRunning ? '检查中…' : seg ? '再次检查' : '开始检查')),
    seg ? diagnosticReadings(seg.steps.map(r => [r.step, r.text])) : foot('尚未运行检查'),
    seg ? foot(`检查耗时 ${seg.ms} ms；浏览器上报不代表实际声音输出。`) : null);
  return [checks, foot('未采集的数据以“—”显示。'), h('div', {class: 'diag-grid'}, events, tests), foot('完整日志与诊断包：手机扫“关于”里的二维码，或在管理页 › 诊断查看。')];
}
