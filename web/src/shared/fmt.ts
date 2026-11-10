// Formatting helpers. Device timestamps are UTC milliseconds; the browser shows local time.
export const pad = (n: number, w = 2) => String(Math.trunc(n)).padStart(w, '0');
export function clock(ms: number, millis = false): string {
  const d = new Date(ms);
  return `${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}${millis ? '.' + pad(d.getMilliseconds(), 3) : ''}`;
}
export function dayClock(ms: number): string {
  const d = new Date(ms), now = new Date();
  const same = d.toDateString() === now.toDateString();
  const yesterday = new Date(now.getTime() - 86_400_000).toDateString() === d.toDateString();
  const hm = `${pad(d.getHours())}:${pad(d.getMinutes())}`;
  return same ? hm : yesterday ? `昨天 ${hm}` : `${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${hm}`;
}
export function duration(seconds: number): string {
  const s = Math.max(0, Math.round(seconds));
  const d = Math.floor(s / 86400), h = Math.floor(s / 3600) % 24, m = Math.floor(s / 60) % 60;
  if (d) return `${d} 天 ${h} 小时 ${m} 分`;
  if (h) return `${h} 小时 ${m} 分`;
  if (m) return `${m} 分钟`;
  return `${s % 60} 秒`;
}
export function mmss(seconds: number): string {
  const s = Math.max(0, Math.floor(seconds));
  return s >= 3600 ? `${Math.floor(s / 3600)}:${pad(Math.floor(s / 60) % 60)}:${pad(s % 60)}` : `${pad(Math.floor(s / 60))}:${pad(s % 60)}`;
}
export function offset(ms: number): string {
  const s = ms / 1000;
  return s < 60 ? `+${s.toFixed(1)} s` : `+${mmss(s)}`;
}
export const num = (v: number | null | undefined, digits = 0, unit = '') => v === null || v === undefined || !Number.isFinite(v) ? '—' : `${v.toFixed(digits)}${unit}`;
export function bytes(n: number | null | undefined): string {
  if (n === null || n === undefined) return '—';
  if (n >= 1 << 20) return `${(n / (1 << 20)).toFixed(1)} MB`;
  if (n >= 1024) return `${(n / 1024).toFixed(0)} KB`;
  return `${n} B`;
}
export const kb = (n: number | null | undefined) => (n === null || n === undefined ? '—' : bytes(n * 1024));
export function range(values: (number | null)[], digits = 0): string {
  const v = values.filter((x): x is number => x !== null && Number.isFinite(x));
  if (!v.length) return '—';
  const lo = Math.min(...v), hi = Math.max(...v);
  return lo.toFixed(digits) === hi.toFixed(digits) ? lo.toFixed(digits) : `${lo.toFixed(digits)}–${hi.toFixed(digits)}`;
}
export const PROTOCOL_TEXT: Record<string, string> = {
  BROWSER_FRAME_RENDERED: '车机已绘制画面',
  BROWSER_DISPLAY_ERROR: '车机画面出错',
  WAITING_BROWSER_REPORT: '等待车机绘制',
};
