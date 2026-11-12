// Tiny DOM builder. Text is always set through textContent, never innerHTML, because log lines,
// hostnames and config values come from the device and must not be interpreted as markup.
type Child = Node | string | number | null | undefined | false;
type Attrs = Record<string, string | number | boolean | null | undefined | EventListener>;

export function h<K extends keyof HTMLElementTagNameMap>(tag: K, attrs: Attrs = {}, ...children: Child[]): HTMLElementTagNameMap[K] {
  const el = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs)) {
    if (v === null || v === undefined || v === false) continue;
    if (k.startsWith('on') && typeof v === 'function') el.addEventListener(k.slice(2).toLowerCase(), v as EventListener);
    else if (k === 'class') el.className = String(v);
    else if (k === 'text') el.textContent = String(v);
    else if (v === true) el.setAttribute(k, '');
    else el.setAttribute(k, String(v));
  }
  append(el, children);
  return el;
}
export function append(el: Element, children: Child[]): void {
  for (const c of children) {
    if (c === null || c === undefined || c === false) continue;
    el.append(typeof c === 'string' || typeof c === 'number' ? document.createTextNode(String(c)) : c);
  }
}
export function clear(el: Element, ...children: Child[]): void {
  const active = document.activeElement as HTMLElement | null;
  const focusId = active && el.contains(active) ? active.id : '';
  el.replaceChildren();
  append(el, children);
  if (focusId) document.getElementById(focusId)?.focus({preventScroll: true});
}
export function radioKeys(event: Event): void {
  const e = event as KeyboardEvent;
  if (!['ArrowLeft', 'ArrowRight', 'ArrowUp', 'ArrowDown'].includes(e.key)) return;
  const buttons = [...(e.currentTarget as HTMLElement).querySelectorAll<HTMLButtonElement>('[role="radio"]:not(:disabled)')];
  const index = buttons.indexOf(e.target as HTMLButtonElement);
  if (index < 0) return;
  e.preventDefault();
  const next = buttons[(index + (e.key === 'ArrowLeft' || e.key === 'ArrowUp' ? -1 : 1) + buttons.length) % buttons.length];
  next.focus(); next.click();
}
export const $ = <T extends HTMLElement = HTMLElement>(id: string) => document.getElementById(id) as T;

const SVG = 'http://www.w3.org/2000/svg';
/** Feather-style stroke icon from path data. */
export function icon(name: keyof typeof ICONS, size = 24, stroke = 2): SVGSVGElement {
  const svg = document.createElementNS(SVG, 'svg');
  svg.setAttribute('width', String(size));
  svg.setAttribute('height', String(size));
  svg.setAttribute('viewBox', '0 0 24 24');
  svg.setAttribute('fill', 'none');
  svg.setAttribute('stroke', 'currentColor');
  svg.setAttribute('stroke-width', String(stroke));
  svg.setAttribute('stroke-linecap', 'round');
  svg.setAttribute('stroke-linejoin', 'round');
  svg.setAttribute('aria-hidden', 'true');
  for (const [kind, data] of ICONS[name]) {
    const node = document.createElementNS(SVG, kind);
    for (const [k, v] of Object.entries(data)) node.setAttribute(k, v);
    svg.append(node);
  }
  return svg;
}
type Shape = [string, Record<string, string>];
const p = (d: string): Shape => ['path', { d }];
const pl = (points: string): Shape => ['polyline', { points }];
export const ICONS = {
  power: [p('M18.36 6.64a9 9 0 1 1-12.73 0'), p('M12 2v10')],
  sliders: [p('M4 21v-7'), p('M4 10V3'), p('M12 21v-9'), p('M12 8V3'), p('M20 21v-5'), p('M20 12V3'), p('M1 14h6'), p('M9 8h6'), p('M17 16h6')],
  expand: [p('M8 3H5a2 2 0 0 0-2 2v3'), p('M21 8V5a2 2 0 0 0-2-2h-3'), p('M3 16v3a2 2 0 0 0 2 2h3'), p('M16 21h3a2 2 0 0 0 2-2v-3')],
  refresh: [pl('23 4 23 10 17 10'), p('M20.49 15a9 9 0 1 1-2.12-9.36L23 10')],
  monitor: [['rect', { x: '2', y: '4', width: '20', height: '14', rx: '2' }], p('M8 21h8'), p('M12 18v3')],
  moon: [p('M21 12.79A9 9 0 1 1 11.21 3 7 7 0 0 0 21 12.79z')],
  mic: [['rect', { x: '9', y: '2', width: '6', height: '12', rx: '3' }], p('M5 10v1a7 7 0 0 0 14 0v-1'), p('M12 18v4')],
  left: [pl('15 18 9 12 15 6')],
  right: [pl('9 18 15 12 9 6')],
  phone: [['rect', { x: '6', y: '2', width: '12', height: '20', rx: '3' }], p('M11 18h2')],
  activity: [pl('22 12 18 12 15 21 9 3 6 12 2 12')],
  check: [pl('20 6 9 17 4 12')],
  logo: [p('M4 16l4-8 4 6 3-4 5 6')],
  bluetooth: [pl('6.5 6.5 17.5 17.5 12 23 12 1 17.5 6.5 6.5 17.5')],
  link: [p('M10 13a5 5 0 0 0 7.54.54l3-3a5 5 0 0 0-7.07-7.07l-1.72 1.71'), p('M14 11a5 5 0 0 0-7.54-.54l-3 3a5 5 0 0 0 7.07 7.07l1.71-1.71')],
} satisfies Record<string, Shape[]>;
