import {h, radioKeys} from '../shared/dom';

export const group = (...rows: (Node | null)[]) => h('section', {class: 'group'}, ...rows);
export const label = (s: string) => h('div', {class: 'g-label'}, s);
export const foot = (s: string) => h('p', {class: 'foot'}, s);
export const text = (title: string, sub?: string) => h('span', {class: 'text'}, h('span', {}, title), sub ? h('small', {}, sub) : null);
export const row = (key: string, value: string, mono = false) => h('div', {class: 'row'}, h('span', {class: 'grow'}, key), h('span', {class: `value${mono ? ' mono' : ''}`}, value));
export const button = (name: string, fn: () => void, cls = 'blue') => h('button', {type: 'button', class: `row ${cls}`, onclick: fn}, name);
const confirmations = new Map<string, number>();
export function confirmButton(name: string, fn: () => void, options: {class?: string; confirmText?: string} = {}): HTMLElement {
  const armed = (confirmations.get(name) ?? 0) > Date.now();
  const confirmText = options.confirmText ?? `再点一次确认${name.replace(' CarPlay', '')}`;
  const b = h('button', {type: 'button', class: `${options.class ?? 'row red'}${armed ? ' armed' : ''}`}, armed ? confirmText : name);
  b.onclick = () => {
    if ((confirmations.get(name) ?? 0) > Date.now()) { confirmations.delete(name); fn(); return; }
    confirmations.set(name, Date.now() + 4000); b.classList.add('armed'); b.textContent = confirmText;
    setTimeout(() => { if ((confirmations.get(name) ?? 0) <= Date.now()) { confirmations.delete(name); b.classList.remove('armed'); b.textContent = name; } }, 4000);
  };
  return b;
}
export function segment<T extends string>(name: string, options: [T, string][], selected: T, pick: (v: T) => void): HTMLElement {
  return h('div', {class: 'segments', role: 'radiogroup', 'aria-label': name, onkeydown: radioKeys}, ...options.map(([v, title]) => h('button', {type: 'button', id: `phone-${name}-${v}`, role: 'radio', tabindex: v === selected ? 0 : -1, 'aria-checked': String(v === selected), onclick: () => pick(v)}, title)));
}
export function toggle(title: string, on: boolean, fn: () => void, disabled = false): HTMLElement {
  return h('button', {type: 'button', class: 'row', role: 'switch', disabled, 'aria-checked': String(on), onclick: fn}, h('span', {class: 'grow'}, title), h('span', {class: 'switch'}, h('i')));
}
