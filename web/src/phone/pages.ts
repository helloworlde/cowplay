import {receiverName, receiverPlatform, receiverDevice} from '../shared/receiver';
import {h, icon} from '../shared/dom';
import {displaySupport} from '../shared/display-limits';
import {diagnosticMeasurements, diagnosticReadings} from '../shared/diagnostics';
import {linkBusy, phoneModel, phoneWifi, recentLinkError, targetPhone, type Phone, type Quality} from '../shared/api';
import {num, clock, duration} from '../shared/fmt';
import {ago, browserName} from '../car/ui';
import {app, prefs, type Actions} from './app';
import {button, foot, group, label, row, segment, text, toggle} from './ui';

const qualities: [Quality, string][] = [['auto', '自动'], ['smooth', '流畅'], ['sharp', '清晰'],['custom','自定义']];
const qNames = {auto: '自动', smooth: '流畅', sharp: '清晰',custom:'自定义'};
export function status(a: Actions): Node[] {
  const s = app.state!, st = app.stats, p = prefs(), target = targetPhone(s), list = s.phones ?? [];
  const live = s.hub.phone && s.hub.viewer && s.hub.display === 'BROWSER_FRAME_RENDERED', err = recentLinkError(s);
  const lead = live ? '正在投屏，开车时不用看手机。'
    : s.stopped ? 'CarPlay 已断开，配对都已保留。点“连接 CarPlay”或选择一台 iPhone。'
    : err ? err.title : s.hub.phone ? 'iPhone 已连接，等待车机显示画面。' : linkBusy(s) ? `正在连接 ${target?.name ?? 'iPhone'}…` : list.length ? '尚未连接 iPhone，可以在下方连接或切换。' : '还没有配对的 iPhone，先在“iPhone”里添加。';
  const nodes = [
    ['车', '车机', s.viewer?.name ?? '尚未连接', !!s.hub.viewer],
    ['主机', receiverDevice(s), `${num(st?.f50.cpu, 0, '%')} · ${num(st?.f50.temp, 0, '°C')}`, true],
    ['iP', target?.name ?? 'iPhone', s.hub.phone ? 'CarPlay 已连接' : s.stopped ? '已断开' : s.link.bluetooth_detail || '未连接', !!s.hub.phone],
  ] as const;
  const chain = h('div', {class: 'chain'});
  nodes.forEach(([, name, sub, ok], i) => {
    chain.append(h('div', {class: `node${i === 1 ? ' receiver-node' : ''}`}, h('span', {class: `node-icon ${ok ? 'ok' : 'idle'}`}, icon(i===0?'monitor':i===1?'link':'phone',22)), h('b', {title: name}, name), h('small', {title: sub}, sub)));
    if (i < 2) chain.append(h('span', {class: `chain-line${(i === 0 ? s.hub.viewer : s.hub.phone) ? ' ok' : ''}`}));
  });
  const spec=s.spec,actual=st&&s.session&&st.session?.id===s.session.id&&Math.abs(s.now_ms-st.ts_ms)<5000?st.car:null;
  const detail = live && spec ? `正在投屏 · ${actual?.width&&actual?.height?`${actual.width}×${actual.height}`:'实际尺寸待上报'} · ${actual?.fps==null?'帧率待采样':`${actual.fps.toFixed(1)} fps`}${s.session ? ` · 已 ${duration((s.now_ms - s.session.started_ms) / 1000)}` : ''}` : s.hub.phone ? 'CarPlay 已连接 · 等待车机画面' : s.link.error?.hint ?? '等待 iPhone 连接';
  // A healthy projection needs no call to action; reconnecting stays available in the screen list.
  const reconnect = () => void a.cmd('reconnect');
  const primary = live ? null : s.hub.phone ? button('重连 CarPlay', reconnect, 'status-primary') : list.length ? button(`连接 CarPlay${target ? ` · ${target.name}` : ''}`, () => void a.cmd('connect', target?.id), 'status-primary') : null;
  return [h('p', {class: 'lead'}, lead), ...(primary ? [primary] : []), h('section', {class: 'link-card'}, chain, h('div', {class: 'live'}, h('i', {class: live ? 'ok' : 'idle'}), detail)),
    label('iPhone'), group(h('button', {type: 'button', class: 'row', 'aria-label': '切换 iPhone', onclick: () => { location.hash = 'iphones'; }},
      text(target?.name ?? '还没有 iPhone', target ? (target.current ? '正在投屏' : s.stopped ? '已断开' : linkBusy(s) ? '正在连接…' : target.preferred ? '首选' : '已保存') : '点这里添加'),
      h('span', {class: 'value'}, list.length > 1 ? `切换 · ${list.length} 台` : list.length ? '管理' : '添加', icon('right', 16)))),
    label('车机屏幕'), group(live ? button('重连 CarPlay', reconnect) : null, button('刷新车机页面', () => void a.cmd('reloadCar')), s.stopped ? null : button('断开 CarPlay', () => void a.cmd('disconnect'), 'red')),
    foot(s.hub.phone ? '画面持续不更新时可尝试重连；没有声音时先检查播放状态、音量与输出设备。重连保留配对。' : '断开后车机不会自动重连，直到这里或车机上选择连接。'), label('快速设置'), group(
      h('div', {class: 'row quick-quality'}, h('span', {class: 'grow'}, '画质'), p ? segment('画质', qualities, p.quality, v => {if(v==='custom'){app.pref='quality';app.sheet='prefs';a.render();}else void a.prefs({quality:v});}) : h('span', {class: 'value'}, '等待车机屏幕')),
      p ? toggle('车机显示调试信息', p.debug, () => void a.prefs({debug: !p.debug})) : row('车机显示调试信息', '—'))];
}

/** A saved phone can be explicitly called even when its Bluetooth link is offline. */
function phoneRow(p: Phone, a: Actions, live: boolean): HTMLElement {
  const sub = `${p.current ? '正在投屏' : p.online ? '在线' : `离线${p.last_ms ? ` · ${ago(p.last_ms)}` : ''}`}${p.preferred ? ' · 首选' : ''}`;
  const minus = app.edit ? h('button', {type: 'button', class: 'minus', disabled: p.current, 'aria-label': `忘记${p.name}`, onclick: () => void a.cmd('forgetPhone',p.id)}, '−') : null;
  const select = live ? h('button', {type: 'button', class: 'tint', onclick: () => void a.cmd('switchPhone', p.id)}, '切换')
    : h('button', {type: 'button', class: 'tint', onclick: () => void a.cmd('connect', p.id)}, '连接');
  return h('div', {class: 'row phone-row'}, minus, h('span', {class: `phone-icon${p.online ? ' online' : ''}`}, icon('phone', 20)), text(p.name, sub), p.current ? h('span', {class: 'current'}, '投屏中') : app.edit ? (!p.preferred ? h('button', {type: 'button', class: 'tint', onclick: () => void a.cmd('setPreferred', p.id)}, '设为首选') : null) : select);
}
export function iphones(a: Actions): Node[] {
  const s = app.state!, list = s.phones ?? [];
  return [group(...list.map(p => phoneRow(p, a, s.hub.phone || linkBusy(s))), button('＋ 添加 iPhone', a.add)), foot(s.phones_error ? `暂时读不到 iPhone：${s.phones_error}` : app.edit ? '正在投屏的 iPhone 不能忘记，先切到另一台。' : s.hub.phone ? '切换前会提示断开当前 CarPlay；连接时间取决于手机响应。' : '点“连接”会呼叫那台 iPhone；上车默认连首选。点“编辑”可设为首选或忘记。'),
    label('自动连接'), group(row('首选', list.find(p => p.preferred)?.name ?? '—'), toggle(app.policyBusy ? '自动连接设置正在保存…' : '多台在线时在车机上询问', s.policy.ask_when_multi, () => void a.autoConnect(!s.policy.ask_when_multi), app.policyBusy)), foot(`关闭后直接连首选；首选不在时按最近使用顺序。${s.policy.ask_timeout ? `车机上 ${s.policy.ask_timeout} 秒没人选也会自动连接。` : '车机上将等待手动选择。'}`)];
}

export function diagnose(a: Actions): Node[] {
  const s = app.state!, d = app.diag, st = app.stats;
  if (!d) return [foot('正在读取诊断…')];
  const checks = diagnosticMeasurements(d.checks);
  const events = group(...(d.events.length ? d.events.slice(0, 4).map(e => h('div', {class: 'row event'}, h('i', {class: `dot ${e.level}`}), text(e.title, e.cause), h('time', {}, clock(e.ts_ms).slice(0, 5)))) : [row('近期事件', '暂无')]));
  const b = h('button', {type: 'button', class: 'row blue', disabled: app.busy, onclick: a.test}, app.busy ? '检查中…' : '运行连接检查');
  return [
    group(h('button', {type: 'button', class: 'row', 'aria-label': '日志', onclick: () => { location.hash = 'log'; }}, h('span', {class: 'grow'}, '日志'), h('span', {class: 'value'}, app.warnings == null ? '查看日志' : `${app.warnings} 条警告`, icon('right', 16))), b, button('导出诊断包…', a.export)),
    ...(app.segments ? [group(diagnosticReadings(app.segments.steps.map(x => [x.step, x.text]))), foot(`检查耗时 ${app.segments.ms} ms；浏览器上报不代表实际声音输出。`)] : []),
    label('实时数据'), checks, foot('未采集的数据以“—”显示。'), label('事件记录'), events,
    label('车机'), group(h('div', {class: 'row'}, h('span', {class: 'grow'}, '浏览器'), h('span', {class: 'value', title: s.viewer?.ua ?? ''}, s.viewer ? browserName(s.viewer.ua) : '尚未连接')), row('画面区', s.viewer?.window ? `${s.viewer.window.rw}×${s.viewer.window.rh} · DPR ${s.viewer.window.dpr}` : '—', true), row('解码', st?.car?.decoder ?? '—')),
    label(receiverName(s)), group(row('系统', app.system?.os ?? app.system?.openwrt ?? receiverPlatform(s)), row('CowPlay', `${s.version} · 运行 ${duration(s.uptime_s)}`), row('内存可用', `${num(st?.f50.mem_free_mb)} / ${num(st?.f50.mem_total_mb)} MB`, true), row('热点', `${s.network.ssid ?? '—'}${s.network.channel ? ` · CH${s.network.channel}` : ''}`)),
    label('iPhone'), group(row('名称', s.phones?.find(x => x.current)?.name ?? '未连接'), row('型号 · 系统', phoneModel(st)), row('Wi‑Fi 信号', phoneWifi(st)), row('蓝牙', st?.phone.bt ?? '—', true)),
    foot(`诊断码 ${s.diag_code}。诊断包不含配对密钥、访问码与 Wi‑Fi 密码。`)];
}

export function logs(a: Actions): Node[] {
  const switchLog = (v: 'summary' | 'raw') => { app.log = v; a.render(); a.logs(); };
  return [segment('日志视图', [['summary', '摘要'], ['raw', '原始']], app.log, switchLog),
    ...(app.log === 'summary' ? [group(...(app.events.length ? app.events.map(e => h('div', {class: 'row event'}, h('i', {class: `dot ${e.level}`}), text(e.title, e.detail), h('time', {}, clock(e.ts_ms).slice(0, 5)))) : [row('日志摘要', '暂无记录')])), foot('按服务端事件记录显示；颜色对应日志级别。')] : [
      h('div', {class: 'log-filter'}, ...[['all', '全部'], ['warn', '警告'], ['error', '错误']].map(([v, title]) => h('button', {type: 'button', class: app.level === v ? 'selected' : '', 'aria-pressed': String(app.level === v), onclick: () => { app.level = v; a.logs(); }}, title))),
      h('pre', {class: 'raw-log'}, ...app.lines.map(l => h('div', {}, h('span', {class: 'log-time'}, clock(l.ts_ms)), ' ', h('strong', {class: `log-level ${l.level.toLowerCase()}`}, l.level.toUpperCase()), ' ', h('span', {}, l.msg ? `${l.target ? `${l.target}: ` : ''}${l.msg}` : l.text))))] )];
}

export function settings(a: Actions): Node[] {
  const s = app.state!, p = prefs();
  const pref = (key: typeof app.pref, title: string, value: string) => h('button', {type: 'button', id: `phone-pref-${key}`, class: 'row', disabled: !p, onclick: () => { app.pref = key; app.sheet = 'prefs'; a.render(); }}, h('span', {class: 'grow'}, title), h('span', {class: 'value'}, value, icon('right', 16)));
  return [label('投屏'), group(pref('quality', '画质', p ? qNames[p.quality] : '—'), pref('call', '通话麦克风', p ? p.call === 'bt' ? '不使用屏幕麦克风' : '屏幕麦克风' : '—'), pref('rail_side', '控制栏位置', p ? p.rail_side === 'left' ? '左侧' : '右侧' : '—')),
    foot('投屏偏好跟随当前车机屏幕，和车机“设置”里是同一份。'),
    group(row('访问码', app.showToken && app.token ? app.token : '••••••', true), h('button', {type:'button',class:'row blue',disabled:app.tokenBusy,onclick:a.token}, app.tokenBusy ? '正在读取访问码…' : app.showToken ? '隐藏访问码' : '显示访问码')), foot('访问码可选：开启后新浏览器验证一次，未启用时可直接进入。开启、修改或停用请到管理页。'),
    label('由管理页负责（只读）'), group(row('CarPlay 热点', s.network.ssid ?? '—'), row('访问地址', s.network.host, true), h('div', {class: 'row'}, h('span', {class: 'grow'}, 'HTTPS 证书'), h('span', {class: `value${s.network.cert_days == null ? '' : s.network.cert_days >= 0 ? ' green' : ' red'}`}, s.network.cert_days == null ? '—' : s.network.cert_days === 0 ? '有效 · 不足 1 天' : s.network.cert_days > 0 ? `有效 · 剩 ${s.network.cert_days} 天` : '已过期')), row('DDNS', s.network.ddns.enabled ? s.network.ddns.ok === false ? '更新失败' : s.network.ddns.ok ? '已更新' : '等待更新' : '未启用')),
    group(h('a', {class: 'row', href: s.network.luci ?? '#', target: '_blank', rel: 'noopener'}, text('打开管理页', s.network.platform === 'linux' ? 'CowPlay 主机管理' : 'OpenWrt › 服务 › CowPlay'), h('span', {class: 'value'}, '↗')), row('关于 CowPlay', s.version)),
    displaySupport(s), foot(s.network.platform === 'linux' ? 'CowPlay 管理使用独立管理令牌。热点、路由与服务由主机系统管理。' : 'Wi‑Fi、证书、DDNS、访问码、服务和升级都在管理页修改，使用 OpenWrt 的登录账号。')];
}
