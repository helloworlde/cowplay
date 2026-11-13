import {receiverDevice} from '../shared/receiver';
import {confirmAction} from '../shared/confirmation';
// 01 · Connect flow: onboard, add iPhone, pair, connecting, choose, takeover, not found, token, stopped.
import {$, clear, h} from '../shared/dom';
import {mmss} from '../shared/fmt';
import {qrSvg} from '../shared/qr';
import {pairingAdapterName, pairingCode, pairingFinishing, pairingWaiting, recentLinkError, targetPhone, type Phone} from '../shared/api';
import type {NearbyPhones} from '../shared/pairing';
import {ConnectionDebug} from '../shared/connection-debug';
import {app, phoneName, phonesOf, type Scene} from './app';
import {spaced, svg, ago, renderManagementEntry} from './ui';

export interface ConnectActions {
  /** Connect (or switch to) this saved iPhone. */
  pick(id: string): void;
  openChoose(): void;
  retry(): void;
  takeoverNow(): void;
  keep(): void;
  /** Open a pairing window for another iPhone. */
  addPhone(): void;
  /** Leave the add-iPhone scene; closes a window that is still open. */
  cancelAdd(): void;
  token(code: string, confirmation?: string): Promise<string | null>;
  openAccess(): Promise<string | null>;
  nearby: NearbyPhones;
}

const NOT_FOUND_MS = 20_000;
const debug=new ConnectionDebug(()=>app.authorized?app.state?.me.id??'':'');

export function scene(): Scene {
  const s = app.state;
  if (!app.authorized) return 'token';
  if (!s) return 'connecting';
  // An acknowledged stop wins over an older cached pairing snapshot.
  if (app.stopped || s.stopped) return 'stopped';
  const p = s.pairing;
  if (pairingCode(p)) return 'pair';
  if (pairingFinishing(p)) return 'finishing';
  const phones = phonesOf(s);
  if (app.addPhone || p.pairing_error) return 'window';
  if (!phones.length && !s.phones_error) return 'onboard';
  if (pairingWaiting(p)) return 'window';
  if (app.takeover) return 'takeover';
  if (app.choose) return 'choose';
  if (notFound()) return 'notfound';
  return 'connecting';
}
export function notFound(): boolean {
  const s = app.state;
  if (!s || !app.connectingSince || s.hub.phone || recentLinkError(s) || s.pairing.pairing_error || s.link.wifi === 'active' || s.link.carplay === 'active') return false;
  return Date.now() - app.connectingSince > NOT_FOUND_MS && s.link.bluetooth !== 'ok';
}

type St = 'ok' | 'active' | 'warn' | 'idle';
function node(kind: 'car' | 'f50' | 'phone', label: string, sub: string, st: St): HTMLElement {
  return h('div', {class: `node ${st} st-${st}${kind === 'f50' ? ' receiver-node' : ''}`},
    h('span', {class: 'ring'}, svg(kind), h('span', {class: 'badge'})),
    h('span', {class: 'node-label'}, label), h('span', {class: 'node-sub'}, sub));
}
function link(metric: string, st: St): HTMLElement {
  return h('div', {class: `plink st-${st}${st === 'ok' ? ' solid' : ''}`}, h('span', {class: 'plink-metric'}, metric), h('span', {class: 'plink-line'}));
}
type StepView = {title: string; st: 'done' | 'active' | 'todo' | 'fail'};
const STEP_NAMES = ['蓝牙', 'Wi‑Fi', 'CarPlay', '画面'];
export function steps(): StepView[] {
  const s = app.state;
  if (!s || pairingFinishing(s.pairing)) return STEP_NAMES.map((title, i) => ({title, st: i ? 'todo' : 'active'}));
  const l = s.link;
  const raw: ('ok' | 'active' | 'fail' | 'idle')[] = [
    s.hub.phone || l.bluetooth === 'ok' ? 'ok' : l.bluetooth,
    s.hub.phone || l.wifi === 'ok' ? 'ok' : l.wifi,
    s.hub.verified || l.carplay === 'ok' ? 'ok' : l.carplay,
    app.rendered ? 'ok' : s.hub.video ? 'active' : 'idle',
  ];
  const first = raw.findIndex(x => x !== 'ok');
  return STEP_NAMES.map((title, i) => ({
    title,
    st: raw[i] === 'ok' ? 'done' : raw[i] === 'fail' ? 'fail' : i === first ? 'active' : 'todo',
  }));
}
function stepList(): HTMLElement {
  return h('ol', {class: 'steps', 'aria-label': '连接进度'}, ...steps().map(x => h('li', {class: x.st}, h('span', {class: 'bar'}), h('span', {class: 'lbl'}, x.title))));
}
function rtt(): string {
  const r = app.local.rttMs;
  return r === null ? (app.channel ? '已连接' : '—') : `${Math.round(r)} ms`;
}

function pipeFor(sc: Scene, name: string): HTMLElement[] {
  const car = (st: St = 'ok', sub = '已连接') => node('car', '这块屏幕', sub, st);
  const f50 = node('f50', 'CowPlay', receiverDevice(app.state), 'ok');
  const s = app.state;
  const newName = s?.pairing.name ?? '新 iPhone';
  switch (sc) {
    case 'onboard': return [car(), link(rtt(), 'ok'), f50, link('未配对', 'idle'), node('phone', 'iPhone', '等待配对', 'idle')];
    case 'window': {
      const open = !!s && pairingWaiting(s.pairing);
      return [car(), link(rtt(), 'ok'), f50, link(open ? '可被发现' : '已结束', open ? 'active' : 'idle'), node('phone', '新 iPhone', open ? '等待配对' : '未配对', open ? 'active' : 'idle')];
    }
    case 'pair': return [car(), link(rtt(), 'ok'), f50, link('配对中', 'active'), node('phone', newName, '等待确认', 'active')];
    case 'finishing': return [car(), link(rtt(), 'ok'), f50, link('配对中', 'active'), node('phone', newName, '正在保存配对', 'active')];
    case 'choose': {
      const n = app.choose?.manual ? phonesOf(s).length : phonesOf(s).filter(p => p.online).length;
      return [car(), link(rtt(), 'ok'), f50, link('等你选', 'active'), node('phone', `${n} 台 iPhone`, app.choose?.manual ? '已保存' : '都在附近', 'active')];
    }
    case 'takeover': {
      const fps = app.stats?.server.fps_in;
      return [car('warn', '未显示'), link(rtt(), 'ok'), f50, link(fps ? `${Math.round(fps)} fps` : '投屏中', 'ok'), node('phone', name, 'CarPlay 已连接', 'ok')];
    }
    case 'notfound': return [car(), link(rtt(), 'ok'), f50, link('尚未连接', 'warn'), node('phone', name, '等待响应', 'idle')];
    case 'token': return [car('warn', '需要访问码'), link('—', 'idle'), node('f50', 'CowPlay', receiverDevice(app.state), 'ok'), link('—', 'idle'), node('phone', 'iPhone', '', 'idle')];
    case 'stopped': return [car(), link(rtt(), 'ok'), f50, link('已断开', 'idle'), node('phone', `${phonesOf(s).length} 台 iPhone`, '配对已保留', 'idle')];
    default: {
      const st = steps(), active = st.findIndex(x => x.st !== 'done');
      const metric = ['正在呼叫…', '加入 Wi‑Fi…', '建立 CarPlay…', '等待画面…'][Math.max(0, active)] ?? '已连接';
      const err = !!s && !!recentLinkError(s);
      const sub = s?.hub.phone ? 'CarPlay 已连接' : s?.link.bluetooth === 'ok' ? '蓝牙已认证' : '等待 iPhone';
      return [car(app.channel ? 'ok' : 'active', app.channel ? '已连接' : '连接中'), link(rtt(), app.channel ? 'ok' : 'active'), f50,
        link(err ? '未完成' : metric, err ? 'warn' : 'active'), node('phone', name, sub, err ? 'warn' : 'active')];
    }
  }
}

function btn(text: string, cls: string, onclick: () => void): HTMLElement { return h('button', {type: 'button', class: `btn ${cls}`, onclick}, text); }
const actions = (...b: (HTMLElement | null)[]) => h('div', {class: 'actions'}, ...b);
function qrCard(step: string, title: string, sub: string, text: string | null, aria: string): HTMLElement {
  const box = h('div', {class: 'qr-code', role: 'img', 'aria-label': aria});
  if (text) { try { box.append(qrSvg(text, 300, '#000000', '#FFFFFF')); } catch { box.textContent = '二维码'; } } else box.textContent = '—';
  return h('div', {class: 'qr-card'}, box, h('div', {class: 'qr-text'}, h('span', {class: 'qr-step'}, step), h('span', {class: 'qr-title'}, title), h('span', {class: 'qr-sub'}, sub)));
}

function addRow(a: ConnectActions): HTMLElement {
  return h('button', {type:'button',class:'choice add',onclick:()=>a.addPhone()},h('span',{class:'plus','aria-hidden':'true'},'+'),h('span',{class:'txt'},h('span',{class:'name'},'添加新 iPhone'),h('span',{class:'sub'},'配对另一台 iPhone，已保存的不受影响')));
}
/** Saved iPhones as large rows: tap one to connect it. */
function phoneList(list: Phone[], a: ConnectActions, picked: string | null, deadline: number, add: boolean): HTMLElement {
  const left = deadline ? Math.max(0, Math.ceil((deadline - Date.now()) / 1000)) : 0;
  // Three or more rows go side by side so “添加新 iPhone” and the buttons stay on screen.
  return h('div', {class: `choose${list.length + (add ? 1 : 0) >= 3 ? ' grid' : ''}`, role: 'radiogroup', 'aria-label': '选择 iPhone'}, ...list.map(p => {
    const def = p.id === picked;
    const sub = [p.current ? '正在使用' : p.preferred ? '首选' : p.last_ms ? `上次 ${ago(p.last_ms)}` : '已配对', p.online ? '在线' : '离线'].join(' · ');
    return h('button', {type: 'button', class: 'choice', role: 'radio', 'aria-checked': String(def), onclick: () => a.pick(p.id)},
      h('span', {class: `dot${p.online ? '' : ' off'}`}), h('span', {class: 'txt'}, h('span', {class: 'name'}, p.name), h('span', {class: 'sub'}, sub)),
      def && deadline ? h('span', {class: 'tag'}, `${left} 秒后自动连接`) : h('span', {class: 'tag go'}, '连接'));
  }), add ? addRow(a) : null);
}

export function renderConnect(a: ConnectActions): void {
  if (!app.authorized) debug.reset();
  const sc = scene(), s = app.state, name = phoneName(s);
  document.body.dataset.scene = sc;
  clear($('pipe'), ...pipeFor(sc, name));
  $('pipe').classList.toggle('keep', sc === 'connecting');
  const eb = $('c-eyebrow');
  eb.className = 'eyebrow' + (sc === 'takeover' ? ' green' : sc === 'notfound' ? ' orange' : '');
  const phones = phonesOf(s);
  const adapter = s ? pairingAdapterName(s.pairing) : null;
  const unknownName = '蓝牙名称暂时未知，请展开日志与调试信息查看读取错误；也可以在附近列表中选择 iPhone。';
  const openError = app.openError && Date.now() - app.openError.at < 30_000 ? app.openError.message : null;
  let t: [string, string, string, string];
  const body: HTMLElement[] = [];
  switch (sc) {
    case 'onboard': {
      const p = s!.pairing;
      t = ['第一次使用', '配对你的 iPhone', '依次扫码；新浏览器需输入这台 CowPlay 的访问码。', 'iPhone 和 CowPlay 配对后，以后上车会自动连接'];
      const ssid = s?.network.ssid ?? '—';
      const pairUrl = `${location.origin}/pair`;
      body.push(h('div', {class: 'qrs'},
        qrCard('第 1 步', '加入 Wi‑Fi', `${ssid} · 自动填入密码`, s?.wifi_qr ?? null, '加入 Wi‑Fi 的二维码'),
        qrCard('第 2 步', '打开配对页', `${location.host}/pair`, pairUrl, '打开配对页的二维码')));
      body.push(h('p', {class: 'lead'}, openError ? `配对窗口未打开：${openError}` : adapter ? `也可以直接在 iPhone 的 设置 › 蓝牙 里点 “${adapter}”${p.open && p.remaining_s != null ? `（${mmss(p.remaining_s)}）` : ''}` : unknownName));
      if (pairingWaiting(p) && (p.nearby ?? []).some(d => d.iphone || d.phone)) body.push(Object.assign(a.nearby.view(p, {limit: 2, phonesOnly: true, header: '或点附近的 iPhone：'}), {className: 'nearby-phones inline'}));
      break;
    }
    case 'window': {
      const p = s!.pairing;
      if (pairingWaiting(p)) {
        t = [`添加 iPhone · 剩 ${mmss(p.remaining_s ?? 0)}`, adapter ? `在 iPhone 上点“${adapter}”` : '蓝牙名称暂时未知', adapter ? `打开新 iPhone 的 设置 › 蓝牙，在“其他设备”里点“${adapter}”；也可以直接点下面的 iPhone。` : unknownName, '配对期间不会连接其他 iPhone'];
        const near = a.nearby.view(p, {limit: 4, phonesOnly: true, header: p.discovering ? '附近的手机 · 点一下即可配对' : '附近的手机 · 扫描已停止'});
        near.classList.toggle('grid', near.querySelectorAll('.nearby-phone').length >= 3);
        body.push(near);
        body.push(actions(btn('取消', '', () => a.cancelAdd())));
      } else {
        const err = p.pairing_error ?? openError;
        t = [err ? '配对未完成' : '配对窗口已结束', '还没有添加新 iPhone', err ? `${err.replace(/[。.]$/, '')}。` : '3 分钟内没有 iPhone 完成配对。', '已保存的 iPhone 不受影响'];
        eb.className = 'eyebrow orange';
        body.push(actions(btn('重新开始', 'blue', () => a.addPhone()), btn('返回', '', () => a.cancelAdd())));
      }
      break;
    }
    case 'pair': {
      const p = s!.pairing;
      t = [`“${p.name ?? 'iPhone'}”请求配对`, '核对 iPhone 上的数字', '一致的话，在这里确认，再在 iPhone 上依次点：', '确认后自动连接，不用回到这里'];
      body.push(h('div', {class: 'code'}, spaced(p.passkey ?? '')));
      body.push(h('div', {class: 'actions chips'},
        h('span', {class: 'chip'}, h('span', {class: 'num'}, '1'), '点“配对”'), h('span', {class: 'chip-arrow'}, '›'),
        h('span', {class: 'chip'}, h('span', {class: 'num'}, '2'), '点“使用 CarPlay”')));
      break;
    }
    case 'finishing': {
      const p = s!.pairing;
      t = ['正在完成配对', p.name ?? '新 iPhone', 'iPhone 上若还有“配对”弹窗请点一下；保存后自动连接 CarPlay。', '确认后自动继续，不用回到这里'];
      body.push(stepList(), actions(btn('取消配对', '', () => a.cancelAdd())));
      break;
    }
    case 'choose': {
      const c = app.choose!;
      const list = c.manual ? phones : phones.filter(p => p.online);
      t = c.manual ? [`${list.length} 台已保存的 iPhone`, '连接哪一台？', '离线的 iPhone 需要先打开蓝牙和 Wi‑Fi。', '投屏中也可以在控制中心里切换']
        : [`${list.length} 台已保存的 iPhone 在附近`, '连接哪一台？', '默认连接上次使用的那台。', '投屏中也可以在控制中心里切换'];
      body.push(phoneList(list, a, c.picked, c.deadline, c.manual));
      if (c.manual) body.push(actions(btn('取消', '', () => a.keep())));
      break;
    }
    case 'takeover': {
      t = ['已连接', '画面正在另一块屏幕显示', '可能是手机或另一台车机打开了这个页面。切到这里后，那边会停止显示。', '同一时间只在一块屏幕上显示'];
      const d = app.takeover?.deadline;
      const left = d ? Math.max(0, Math.ceil((d - Date.now()) / 1000)) : null;
      const fill = d ? Math.min(1, (3000 - (d - Date.now())) / 3000) : 0;
      body.push(actions(
        h('button', {type: 'button', class: 'btn count', onclick: () => a.takeoverNow()}, h('i', {class: 'fill', style: `width:${Math.round(fill * 100)}%`}), h('span', {}, left === null ? '在这块屏幕显示' : `在这块屏幕显示（${left}）`)),
        left !== null ? btn('保持现状', '', () => a.keep()) : null));
      break;
    }
    case 'notfound':
      t = ['连接尚未完成', `正在等待 ${name}`, '还没有收到连接确认，请检查手机并展开下方日志查看当前步骤。', '可以重试或选择另一台手机'];
      body.push(h('ul', {class: 'checklist'}, h('li', {}, 'iPhone 的蓝牙和 Wi‑Fi 都已打开'), h('li', {}, 'iPhone 没有连着车自带的蓝牙'), h('li', {}, 'iPhone 在车内，没有被锁在后备箱')));
      body.push(actions(btn('重新查找', 'blue', () => a.retry()), btn('换一台 iPhone', '', () => a.openChoose())));
      break;
    case 'stopped':
      t = ['已断开', '连接哪一台 iPhone？', '配对都已保留，点一台就会连接。', '断开后不会自动重连'];
      // targetPhone() prefers the phone used just before the stop over the preferred one.
      body.push(phoneList(phones, a, targetPhone(s)?.id ?? null, 0, true));
      break;
    case 'token':
      t = ['正在连接', name, '保持 iPhone 的蓝牙和 Wi‑Fi 打开。', '访问码只在这台车机上验证'];
      body.push(h('ol', {class: 'steps'}, ...STEP_NAMES.map(x => h('li', {}, h('span', {class: 'bar'}), h('span', {class: 'lbl'}, x)))));
      break;
    default: {
      const e = app.channelError ? {title: '这块屏幕暂时连不上 CowPlay', hint: app.channelError}
        : s?.phones_error ? {title: '暂时读不到已保存的 iPhone', hint: s.phones_error}
        : s ? recentLinkError(s) : null;
      t = e ? ['连接未完成', name, `${e.title.replace(/[。.]$/, '')}。${e.hint}`, '连上后自动进入 CarPlay']
        // The pipe already reports CarPlay as connected; the headline must agree while the first frame is pending.
        : s?.hub.phone ? ['已连接', name, 'CarPlay 已建立，正在等待第一帧画面。', '画面出现后自动进入 CarPlay']
        : ['正在连接', name, '保持 iPhone 的蓝牙和 Wi‑Fi 打开，通常 10 秒内出现画面。', '连上后自动进入 CarPlay'];
      if (e) eb.className = 'eyebrow orange';
      body.push(stepList());
      body.push(actions(e ? btn(app.buildMismatch ? '刷新页面' : '重新连接', 'blue', () => a.retry()) : null, btn('换一台 iPhone', '', () => a.openChoose())));
    }
  }
  if (app.buildMismatch) {
    t = ['页面已更新', '刷新 CowPlay', '页面与接收端版本不一致，授权仍保留。', '刷新后继续使用'];
    body.splice(0, body.length, btn('刷新页面', 'blue', () => a.retry()));
  }
  eb.textContent = t[0];
  $('c-title').textContent = t[1];
  $('c-lead').textContent = t[2];
  $('c-foot').textContent = t[3];
  renderManagementEntry($('c-admin'), s?.network.luci ?? null, s?.network.platform);
  if (app.authorized && app.stateError) body.push(h('p',{class:'error',role:'alert'},app.stateError));
  if (app.authorized && s) body.push(debug.update(s,app.channel));
  clear($('c-body'), ...body);
  $('token-layer').hidden = sc !== 'token';
  $('token-title').textContent = app.accessOpen ? '进入 CowPlay' : app.setupRequired ? (firstCode ? '确认访问码' : '首次设置访问码') : '输入访问码';
  $('token-reset').hidden = !app.setupRequired;
  $('token-open').hidden = !app.setupRequired && !app.accessOpen;
  $('token-open').textContent = app.setupRequired ? '不设置访问码' : '重新进入';
  $('keys').hidden = $('digits').hidden = app.accessOpen;
}
/** The link diagram yields its space before any button would fall below the screen (run after all inserts). */
export function fitConnect(): void {
  const main = document.querySelector<HTMLElement>('.c-main')!;
  main.classList.remove('tight');
  if (main.scrollHeight > main.clientHeight + 1) main.classList.add('tight');
}

// ---------- token keypad ----------
let code = '';
let firstCode = '';
let busy = false;
let keypadReady = false;
export function setupKeypad(a: ConnectActions): void {
  if (keypadReady) return;
  keypadReady = true;
  const keys = $('keys');
  for (const k of ['1', '2', '3', '4', '5', '6', '7', '8', '9', '', '0', '⌫']) {
    keys.append(h('button', {type: 'button', class: k === '' ? 'blank' : k === '⌫' ? 'del' : '', 'aria-label': k === '⌫' ? '删除' : k || '空', tabindex: k === '' ? '-1' : null, onclick: () => press(k, a)}, k));
  }
  window.addEventListener('keydown', e => {
    if ($('token-layer').hidden) return;
    if (/^\d$/.test(e.key)) press(e.key, a);
    else if (e.key === 'Backspace') press('⌫', a);
  });
  $('token-reset').onclick = () => { firstCode='';code='';drawDigits();$('token-title').textContent='首次设置访问码';$('token-msg').textContent='请设置六位数字访问码'; };
  $('token-open').onclick = async () => {
    if (busy) return;
    busy = true;
    try {
      if (app.setupRequired && !await confirmAction({title:'不设置访问码？',description:'能打开此地址的设备可进入投屏和手机管理。以后可在管理页开启访问码。',confirm:'不设置并进入'})) return;
      const error = await a.openAccess();
      code = firstCode = ''; drawDigits();
      if (error) { $('token-msg').textContent = error; $('token-msg').className = 'err'; }
    } finally { busy = false; }
  };
  drawDigits();
}
function drawDigits(): void { clear($('digits'), ...[0, 1, 2, 3, 4, 5].map(i => h('span', {class: i < code.length ? 'on' : ''}))); }
async function press(k: string, a: ConnectActions): Promise<void> {
  if (busy || app.accessOpen) return;
  if (k === '⌫') code = code.slice(0, -1);
  else if (k && code.length < 6) code += k;
  drawDigits();
  if (code.length < 6) return;
  if(app.setupRequired&&!firstCode){firstCode=code;code='';drawDigits();$('token-title').textContent='确认访问码';$('token-msg').textContent='再输入一次刚设置的访问码';return;}
  if(app.setupRequired&&code!==firstCode){code='';drawDigits();$('token-msg').textContent='两次输入不同，请重新确认，或点击重新设置';return;}
  busy = true;
  const error = await a.token(code, app.setupRequired ? firstCode : undefined);
  busy = false;
  code = '';
  drawDigits();
  const msg = $('token-msg');
  if (error) {
    if(!app.setupRequired){firstCode='';$('token-title').textContent=app.accessOpen?'进入 CowPlay':'输入访问码';$('token-reset').hidden=true;}
    msg.textContent = error; msg.className = 'err';
    $('digits').classList.remove('shake'); void $('digits').offsetWidth; $('digits').classList.add('shake');
  } else {
    msg.textContent = '输入这台 CowPlay 首次设置的访问码；此浏览器验证一次后会记住授权。'; msg.className = '';
  }
}
