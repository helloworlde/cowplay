import {api, pairingAdapterName, type PairingStatus} from './api';
import {h} from './dom';
import './pairing.css';

export interface PairingDecision { generation: number; request: number; accept: boolean }

/** The same code, controls and request snapshot on the phone and car pages. */
export class PairingConfirmation {
  private state: PairingStatus | null = null;
  private access = '';
  private pending = false;
  private blocked = false;
  private submitted = '';
  private error = '';
  private readonly code: HTMLParagraphElement;
  private readonly accept: HTMLButtonElement;
  private readonly reject: HTMLButtonElement;
  private readonly note: HTMLParagraphElement;
  private readonly actions: HTMLElement;

  constructor(private root: HTMLElement, prefix: string, private decide: (body: PairingDecision) => Promise<unknown>,
    private refresh: () => void | Promise<void>, private visibility?: (visible: boolean) => void,
    private context: () => string = () => '') {
    this.access = context();
    root.classList.add('pairing-confirmation');
    this.code = h('p', {id: prefix + 'passkey', class: 'pairing-code code', hidden: true});
    this.accept = h('button', {id: prefix + 'accept', type: 'button', class: 'primary', hidden: true, disabled: true, onclick: () => void this.submit(true)}, '数字一致，确认配对');
    this.reject = h('button', {id: prefix + 'reject', type: 'button', class: 'secondary red', hidden: true, disabled: true, onclick: () => void this.submit(false)}, '拒绝配对');
    this.note = h('p', {class: 'pairing-note', role: 'status', 'aria-live': 'polite'});
    this.actions = h('div', {class: 'pairing-actions'}, this.accept, this.reject);
    root.replaceChildren(h('h3', {}, '蓝牙配对确认'), h('p', {class: 'pairing-hint'}, '核对 iPhone 弹窗与下方的 6 位数字，一致再确认。'), this.code, this.actions, this.note);
    root.hidden = true;
  }
  get element() { return this.root; }
  get visible() { return this.state?.request != null && /^\d{6}$/.test(this.state.passkey ?? ''); }
  placeControls(target = this.actions) { target.append(this.accept, this.reject); }
  private key() { return this.state?.request ? `${this.state.generation}/${this.state.request}` : ''; }
  update(state: PairingStatus, blocked = false) {
    const previous = this.key(), access = this.context();
    this.state = state; this.blocked = blocked;
    if (previous !== this.key() || this.access !== access) { this.error = ''; this.submitted = ''; }
    this.access = access;
    this.paint();
  }
  unavailable() { this.blocked = true; this.paint(); }
  private paint() {
    const visible = !!this.state?.request && /^\d{6}$/.test(this.state.passkey ?? '');
    this.root.hidden = this.code.hidden = this.accept.hidden = this.reject.hidden = !visible;
    this.code.textContent = visible ? this.state!.passkey!.replace(/^(\d{3})(\d{3})$/, '$1 $2') : '';
    this.accept.disabled = this.reject.disabled = this.blocked || this.pending || (!!this.submitted && this.submitted === this.key());
    this.note.textContent = this.error || (this.blocked ? '配对状态暂不可用，正在刷新…' : this.pending ? '正在提交确认…' : this.submitted ? '已确认，请在 iPhone 弹窗里点“配对”。' : '两边都要确认：这里点确认，iPhone 弹窗里点“配对”。');
    this.note.classList.toggle('pairing-error', !!this.error);
    this.visibility?.(visible);
  }
  private async submit(accept: boolean) {
    if (!this.state?.request || this.blocked || this.pending || this.submitted === this.key()) return;
    const key = this.key(), access = this.context();
    const body = {generation: this.state.generation, request: this.state.request, accept};
    this.pending = true; this.error = ''; this.paint();
    try {
      await this.decide(body);
      if (this.key() === key && this.context() === access) this.submitted = key;
    } catch (error) {
      if (this.key() === key && this.context() === access) this.error = (error as Error).message;
    } finally {
      this.pending = false;
      this.paint();
      await this.refresh();
    }
  }
}

export interface NearbyOptions { limit?: number; phonesOnly?: boolean; header?: string }
/** Reuse the same discovered-device list in the add sheet, the standalone pairing page and the car. */
export class NearbyPhones {
  private pending = false;
  private attempted = '';
  private error = '';
  private generation = -1;
  private access = '';
  constructor(private refresh: () => Promise<void>, private render: () => void, private context: () => string = () => '') {}
  view(p: PairingStatus, o: NearbyOptions = {}): HTMLElement {
    if (p.generation !== this.generation || this.access !== this.context()) { this.generation = p.generation; this.access = this.context(); this.error = ''; this.attempted = ''; }
    if (p.pairing_error || /cancelled|expired/i.test(p.phase)) this.attempted = '';
    const name = pairingAdapterName(p), label = name ? `蓝牙名称 ${name}` : '蓝牙名称暂时未知';
    const list = h('section', {class: 'nearby-phones'}, h('p', {class: 'foot'}, o.header ?? (p.discovering ? `正在扫描附近设备 · ${label} · iPhone 优先` : label)));
    if (this.error || p.pairing_error) list.append(h('p', {class: 'error', role: 'alert'}, this.error || p.pairing_error!));
    const devices = (p.nearby ?? []).filter(d => !o.phonesOnly || d.iphone || d.phone).slice(0, o.limit ?? Infinity);
    for (const d of devices) {
      const key = `${p.generation}/${d.id}`;
      list.append(h('button', {type: 'button', class: 'row nearby-phone', disabled: !p.open || this.pending || this.attempted === key || !!p.device || p.request != null,
        onclick: async () => {
          if (this.pending || this.attempted === key) return;
          const access = this.context();
          this.pending = true; this.attempted = key; this.error = ''; this.render();
          try { await api('/api/pairing/pair', {body: {device: d.id, generation: p.generation}}); }
          catch (e) { if (this.generation === p.generation && this.context() === access) { this.error = (e as Error).message; this.attempted = ''; } }
          finally { this.pending = false; await this.refresh(); this.render(); }
        }}, h('span', {class: 'text'}, h('span', {}, d.name), h('small', {}, d.iphone ? 'iPhone' : d.phone ? '手机' : '蓝牙设备')), h('span', {class: 'value'}, this.attempted === key ? '正在配对…' : '配对')));
    }
    if (!devices.length) list.append(h('p', {class: 'foot empty'}, p.discovering ? '尚未发现新设备；请保持 iPhone 的设置 › 蓝牙页面打开。' : '尚未发现设备，可重新扫描。'));
    return list;
  }
}
