import {managementLocation} from '../shared/receiver';
// Car UI helpers: design icons, scale factor, small controls.
import {h} from '../shared/dom';

// Icon markup copied from the design sources (static constants, never device data).
const ICON: Record<string, [string, number, string?]> = {
  logo: ['<path d="M4 16l4-8 4 6 3-4 5 6"></path>', 2.4, '#FFFFFF'],
  car: ['<rect x="2" y="4" width="20" height="13" rx="2.5"></rect><path d="M8 21h8"></path><path d="M12 17v4"></path>', 1.7],
  f50: ['<rect x="3" y="12" width="18" height="8" rx="2.5"></rect><path d="M7 16h.01"></path><path d="M11 16h.01"></path><path d="M8.5 8.5a5 5 0 0 1 7 0"></path><path d="M6 6a8.5 8.5 0 0 1 12 0"></path>', 1.7],
  phone: ['<rect x="6" y="2" width="12" height="20" rx="3"></rect><path d="M11 18h2"></path>', 1.7],
  full: ['<path d="M15 4h5v5"></path><path d="M9 20H4v-5"></path><path d="M20 4l-6.5 6.5"></path><path d="M4 20l6.5-6.5"></path>', 2],
  control: ['<path d="M4 8h9"></path><path d="M19 8h1"></path><circle cx="16" cy="8" r="2.4" fill="currentColor" stroke="none"></circle><path d="M4 16h1"></path><path d="M11 16h9"></path><circle cx="8" cy="16" r="2.4" fill="currentColor" stroke="none"></circle>', 2],
  home: ['<rect x="3" y="3" width="6" height="6" rx="1.4"></rect><rect x="15" y="3" width="6" height="6" rx="1.4"></rect><rect x="3" y="15" width="6" height="6" rx="1.4"></rect><rect x="15" y="15" width="6" height="6" rx="1.4"></rect>', 1.8],
  siri: ['<rect x="9" y="2" width="6" height="13" rx="3"></rect><path d="M5 10v2a7 7 0 0 0 14 0v-2M12 19v3M8 22h8"></path>', 1.8],
  back: ['<path d="M10 5l-7 7 7 7M3 12h18"></path>', 2],
  reconnect: ['<path d="M20 12a8 8 0 1 1-2.34-5.66"></path><path d="M20 4v5h-5"></path>', 2.2],
  'chev-right': ['<polyline points="9 18 15 12 9 6"></polyline>', 2.2],
  'chev-left': ['<polyline points="15 18 9 12 15 6"></polyline>', 2.2],
};
export function svg(name: string, size?: number): SVGSVGElement {
  const [markup, stroke, color] = ICON[name];
  const el = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
  el.setAttribute('viewBox', '0 0 24 24');
  el.setAttribute('fill', 'none');
  el.setAttribute('stroke', color ?? 'currentColor');
  el.setAttribute('stroke-width', String(stroke));
  el.setAttribute('stroke-linecap', 'round');
  el.setAttribute('stroke-linejoin', 'round');
  el.setAttribute('aria-hidden', 'true');
  if (size) { el.setAttribute('width', String(size)); el.setAttribute('height', String(size)); }
  el.innerHTML = markup;
  return el;
}
export function hydrateIcons(root: ParentNode = document): void {
  for (const el of root.querySelectorAll<HTMLElement>('[data-svg]')) el.replaceChildren(svg(el.dataset.svg!, el.id === 'h-chev' ? 20 : undefined));
}

/** Open LuCI separately so returning to the running projection does not reload it. */
export function renderManagementEntry(el: HTMLElement, url: string | null, platform?: string): void {
  const key = `${url ?? ''}/${platform ?? ''}`;
  if (el.dataset.url === key) return;
  el.dataset.url = key;
  el.replaceChildren(url
    ? h('a', {href: url, target: '_blank', rel: 'noopener noreferrer'}, '管理页 ↗', h('small', {}, platform === 'linux' ? '画面上限、证书与访问令牌' : 'Wi‑Fi、证书、DDNS、令牌与服务'))
    : h('span', {}, `管理页地址未知：${managementLocation(platform)}`));
}

/** k = clamp(min(W/1280, H/720), 0.72, 1.3); compact below 560 px height. */
export function scale(): {k: number; w: number; h: number; compact: boolean} {
  const w = window.innerWidth, hgt = window.visualViewport?.height ?? window.innerHeight;
  const k = Math.max(0.72, Math.min(1.3, Math.min(w / 1280, hgt / 720)));
  document.documentElement.style.setProperty('--k', String(k));
  document.documentElement.style.setProperty('--app-h', `${hgt}px`);
  document.body.classList.toggle('compact', hgt < 560);
  return {k, w, h: hgt, compact: hgt < 560};
}

export function seg<T extends string>(cls: string, label: string, options: [T, string][], current: T, pick: (v: T) => void): HTMLElement {
  return h('div', {class: cls, role: 'radiogroup', 'aria-label': label},
    ...options.map(([v, text]) => h('button', {type: 'button', role: 'radio', 'aria-checked': String(v === current), onclick: () => pick(v)}, text)));
}
/** iOS switch row; `cls` picks the row style (trow in the sheet, row in settings). */
export function switchRow(cls: string, title: string, note: string | null, on: boolean, toggle: () => void): HTMLElement {
  return h('button', {type: 'button', class: cls, role: 'switch', 'aria-checked': String(on), onclick: toggle},
    h('span', {class: 't'}, h('span', {}, title), note ? h('small', {}, note) : null), h('span', {class: 'switch'}));
}
export function toast(text: string, ms = 2600): void {
  document.querySelector('.toast')?.remove();
  const t = h('div', {class: 'toast', role: 'status'}, text);
  document.getElementById('app')!.append(t);
  setTimeout(() => t.remove(), ms);
}
/** Two-tap confirmation: first tap arms (red), second tap within 4 s runs. */
export function armed(button: HTMLElement, idle: string, armedText: string, run: () => void): void {
  let timer = 0;
  button.addEventListener('click', () => {
    if (button.classList.contains('armed')) { clearTimeout(timer); button.classList.remove('armed'); button.textContent = idle; run(); return; }
    button.classList.add('armed'); button.textContent = armedText;
    timer = window.setTimeout(() => { button.classList.remove('armed'); button.textContent = idle; }, 4000);
  });
}
export function browserName(ua = navigator.userAgent): string {
  const tesla = /Tesla/i.test(ua) ? 'Tesla · ' : '';
  const m = /(Edg|Chrome|Firefox|Version)\/([\d.]+)/.exec(ua);
  const name = m ? (m[1] === 'Version' ? 'Safari' : m[1] === 'Edg' ? 'Edge' : m[1] === 'Chrome' ? 'Chromium' : m[1]) : '浏览器';
  return `${tesla}${name} ${m?.[2]?.split('.')[0] ?? ''}`.trim();
}
export const even = (n: number) => Math.max(0, Math.floor(n) - (Math.floor(n) % 2));
export const spaced = (code: string) => code.length === 6 ? `${code.slice(0, 3)} ${code.slice(3)}` : code;
export function ago(ms: number | null, now = Date.now()): string {
  if (!ms) return '';
  const s = Math.max(0, (now - ms) / 1000);
  if (s < 90) return '刚刚';
  if (s < 3600) return `${Math.round(s / 60)} 分钟前`;
  if (s < 86400) return `${Math.round(s / 3600)} 小时前`;
  return `${Math.round(s / 86400)} 天前`;
}
export function minutes(s: number): string {
  s = Math.max(0, Math.floor(s));
  const m = Math.floor(s / 60);
  if (m >= 60) return `${Math.floor(m / 60)} 小时 ${m % 60} 分`;
  return m ? `${m} 分 ${s % 60} 秒` : `${s} 秒`;
}
