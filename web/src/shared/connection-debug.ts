import {api, download, pairingAdapterName, targetPhone, type State} from './api';
import {h} from './dom';
import './connection-debug.css';

/** Keep one details node so polling preserves expansion, focus and scroll position. */
export class ConnectionDebug {
  readonly element = h('details', {class: 'connection-debug'});
  private snapshot = h('dl', {class: 'connection-debug-state'});
  private log = h('pre', {class: 'connection-debug-log', tabindex: '0', 'aria-label': '最近连接日志'});
  private message = h('p', {class: 'connection-debug-note', role: 'status'});
  private state: State | null = null;
  private busy = false;
  private last = 0;
  private access = '';
  private request: AbortController | null = null;
  private epoch = 0;
  constructor(private context: () => string) {
    const refresh = h('button', {type: 'button', onclick: () => void this.load(true)}, '刷新日志');
    const save = h('button', {type: 'button', onclick: async () => {
      const access = this.context(), epoch = this.epoch;
      try { await download('/api/diag/bundle'); }
      catch (e) { if (this.context() === access && epoch === this.epoch) this.message.textContent = (e as Error).message; }
    }}, '导出诊断包');
    this.element.append(h('summary', {}, '日志与调试信息'), this.snapshot,
      h('div', {class: 'connection-debug-actions'}, refresh, save), this.message, this.log);
    this.element.addEventListener('toggle', () => { if (this.element.open) void this.load(true); });
  }
  reset(): void {
    this.epoch++; this.request?.abort(); this.request = null; this.busy = false;
    this.state = null; this.last = 0; this.snapshot.replaceChildren(); this.log.textContent = ''; this.message.textContent = '';
  }
  update(state: State | null, channel?: boolean): HTMLElement {
    if (!state || this.access !== this.context()) this.reset();
    this.access = this.context(); this.state = state;
    if (!state) return this.element;
    const p = state.pairing, l = state.link, a = p.adapter;
    const names = {idle: '等待', active: '进行中', ok: '已完成', fail: '失败'};
    const rows: [string, string][] = [
      ['当前手机', targetPhone(state)?.name ?? p.name ?? (p.device ? '新 iPhone' : '—')],
      ['蓝牙名称', pairingAdapterName(p) ?? '未知'],
      ['发现响应名称', a?.eir_name ?? (a?.eir_name === null ? '未包含完整名称' : '未知')],
      ['BlueZ Alias', a?.alias ?? '未知'],
      ['系统蓝牙名称', a?.name ?? '未知'],
      ['蓝牙适配器', a ? `${a.powered ? '已上电' : '未上电'} · ${a.discoverable ? '可被发现' : '未开放发现'} · ${a.pairable ? '可配对' : '配对已关闭'}` : p.adapter_error ?? '未知'],
      ['配对', `${p.phase} · 第 ${p.generation} 次 · ${p.discovering ? '扫描中' : '未扫描'}`],
      ['蓝牙 / iAP2', `${names[l.bluetooth]} · ${l.bluetooth_detail}`],
      ['Wi-Fi', `${names[l.wifi]} · ${l.wifi_detail}`],
      ['CarPlay', `${names[l.carplay]} · ${l.carplay_detail}`],
      ['会话与画面', `会话${state.hub.phone ? '已建立' : '未建立'} · 认证${state.hub.verified ? '通过' : '未完成'} · 媒体${state.hub.video ? '已收到' : '未收到'} · ${state.hub.display}`],
      ...(channel === undefined ? [] : [['网页通道', channel ? '已连接' : '未连接'] as [string, string]]),
      ['状态更新时间', new Date(state.now_ms).toLocaleTimeString([], {hour12: false})],
      ['版本 / 诊断编号', `${state.version} · ${state.build} · ${state.diag_code}`],
    ];
    this.snapshot.replaceChildren(...rows.flatMap(([key, value]) => [h('dt', {}, key), h('dd', {}, value)]));
    if (this.element.open) void this.load();
    return this.element;
  }
  private async load(force = false): Promise<void> {
    if (!this.state || !this.context() || this.busy || (!force && Date.now() - this.last < 2000)) return;
    this.busy = true; const access = this.context(), epoch = this.epoch;
    const request = new AbortController(); this.request = request;
    const timer = setTimeout(() => request.abort(), 5000);
    try {
      const page = await api<{entries: {ts_ms: number; level: string; module?: string; msg: string}[]}>('/api/logs?limit=80', {signal: request.signal});
      if (this.context() !== access || epoch !== this.epoch) return;
      const top = this.log.scrollTop;
      this.log.textContent = [...page.entries].reverse().map(e => `${new Date(e.ts_ms).toLocaleTimeString([], {hour12: false})} ${e.level.toUpperCase()} ${e.module ?? ''} ${e.msg}`).join('\n') || '暂无日志';
      this.log.scrollTop = top;
      this.message.textContent = '最近 80 条日志 · 展开时自动刷新 · 地址已脱敏';
      this.last = Date.now();
    } catch (e) {
      if (this.context() === access && epoch === this.epoch) this.message.textContent = `日志读取失败：${request.signal.aborted ? '请求超时' : (e as Error).message}。可点“刷新日志”重试。`;
    } finally { clearTimeout(timer); if (this.request === request) { this.request = null; this.busy = false; } }
  }
}
