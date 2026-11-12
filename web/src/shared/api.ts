// JSON API client. Authorization is the HttpOnly client cookie set by /api/auth.
export class ApiError extends Error {
  constructor(message: string, readonly status: number, readonly data: any = null) { super(message); }
}
/** Transport/server failures cannot prove whether a save was committed. */
export function saveError(error:unknown):string {
  if(error instanceof ApiError&&error.status>=400&&error.status<500)return `未保存：${error.message}`;
  const reason=error instanceof DOMException&&/Timeout|Abort/.test(error.name)?'请求超时':error instanceof TypeError?'网络响应中断':error instanceof ApiError?error.message:'未收到有效保存结果';
  return `保存结果未确认：${reason}，请核对当前设置后重试`;
}
async function failure(response: Response): Promise<ApiError> {
  let data: any = null;
  try { data = await response.json(); } catch { /* not JSON */ }
  const fallback = response.status === 401 ? '这台设备还没有被授权' : `操作未完成（HTTP ${response.status}）`;
  return new ApiError(data?.error || fallback, response.status, data);
}
export async function api<T = any>(path: string, init: { method?: string; body?: unknown; signal?: AbortSignal } = {}): Promise<T> {
  const method = init.method ?? (init.body !== undefined ? 'POST' : 'GET');
  const response = await fetch(path, {
    method,
    cache: 'no-store',
    credentials: 'same-origin',
    signal: init.signal,
    headers: init.body !== undefined ? { 'Content-Type': 'application/json' } : {},
    body: init.body !== undefined ? JSON.stringify(init.body) : undefined,
  });
  if (!response.ok) throw await failure(response);
  if (response.status === 204 || response.status === 202) return null as T;
  return response.json() as Promise<T>;
}
export async function download(path: string): Promise<{ name: string; size: number }> {
  const response = await fetch(path, { cache: 'no-store', credentials: 'same-origin' });
  if (!response.ok) throw await failure(response);
  const blob = await response.blob();
  const name = /filename="([^"]+)"/.exec(response.headers.get('Content-Disposition') ?? '')?.[1] ?? 'cowplay-download';
  const url = URL.createObjectURL(blob);
  const a = document.createElement('a');
  a.href = url; a.download = name; a.hidden = true;
  document.body.append(a); a.click(); a.remove();
  setTimeout(() => URL.revokeObjectURL(url), 10_000);
  return { name, size: blob.size };
}
export const sendCmd = (cmd: string, id?: string, signal?: AbortSignal, generation?:number) => api('/api/cmd', { body: {cmd,...(id?{id}:{}),...(generation!==undefined?{generation}:{})}, signal });

// ---------- authorization ----------
export interface AuthStatus { authorized: boolean; required: boolean; setup_required?: boolean; locked_s: number | null; client: ClientInfo | null }
export interface ClientInfo { id: string; kind: 'screen' | 'phone'; name: string; ua: string; ip: string; last_ms: number; prefs: ScreenPrefs; window: WindowInfo | null; viewing?: boolean }
export const authStatus = () => api<AuthStatus>('/api/auth');
/** Each browser gets its own cookie; a code is required only when explicitly enabled. */
export const admit = (kind: 'screen' | 'phone', extra: { token?: string; name?: string } = {}) => api<{ authorized: boolean; client: ClientInfo }>('/api/auth', { body: { kind, ...extra } });

export const setupAccess = (kind: 'screen' | 'phone', token: string, confirmation: string, name: string) => api('/api/auth/setup', {body: {kind, token, confirmation, name}});
export const setupOpenAccess = (kind: 'screen' | 'phone', name: string) => api('/api/auth/setup', {body: {mode: 'open', kind, name}});

// ---------- shared state ----------
export type Quality = 'auto' | 'smooth' | 'sharp' | 'custom';
export type Resolution = 'native' | '1.5x' | '720p' | '1080p' | '1440p' | '2160p' | `${number}x${number}`;
export interface ScreenPrefs { quality: Quality; resolution: Resolution; fps: number; codec: 'h264' | 'hevc'; call: 'bt' | 'web'; rail_side: 'left' | 'right'; ui_size: 'small' | 'std' | 'large'; right_hand_drive: boolean; night: boolean; debug: boolean }
export interface WindowInfo { w: number; h: number; dpr: number; rw: number; rh: number }
/** `selected`: the phone the receiver calls (or last called). `online`: Bluetooth currently connected. */
export interface Phone { id: string; name: string; online: boolean; current: boolean; selected?: boolean; preferred: boolean; bt: string; last_ms: number | null }
export interface NearbyDevice { id: string; name: string; iphone: boolean; phone: boolean; rssi: number | null }
export interface PairingStatus {
  phase: string; generation: number; request: number | null; passkey: string | null; paired: boolean;
  open: boolean; remaining_s: number | null; device: string | null; name: string | null;
  discovering?: boolean; adapter_name?: string | null; pairing_error?: string | null; nearby?: NearbyDevice[];
  adapter?: {alias:string;name:string;controller_name?:string|null;eir_name?:string|null;powered:boolean;discoverable:boolean;pairable:boolean} | null;
  adapter_error?: string | null;
}
/** New servers report HCI readback; retain compatibility with older BlueZ-only responses. */
export const pairingAdapterName = (p: PairingStatus) => {
  if (p.adapter && 'controller_name' in p.adapter) return p.adapter.controller_name?.trim() || null;
  return p.adapter?.alias.trim() || p.adapter_name?.trim() || null;
};
export const pairingActive = (p: PairingStatus) => p.open || p.request != null || /waiting|confirm|pairing/i.test(p.phase);
/** The six digits are waiting for a decision on this page and on the iPhone. */
export const pairingCode = (p: PairingStatus) => p.request != null && /^\d{6}$/.test(p.passkey ?? '');
/** Both ends confirmed; BlueZ is finishing the bond. */
export const pairingFinishing = (p: PairingStatus) => /^pairing$/i.test(p.phase);
/** The window is open and no iPhone has asked yet. */
export const pairingWaiting = (p: PairingStatus) => p.open && /^waiting$/i.test(p.phase);
/** A phone call is in progress: the service reports an iPhone mid hand-off (it may wait at “使用
 * CarPlay？”, bounded), or a step is active and changed within the last minute. Some service paths
 * leave a step “active” (e.g. scanning after a window expired); that must not block retries forever. */
export const linkBusy = (s: State) => {
  const l = s.link;
  return !!s.handoff || ((l.bluetooth === 'active' || l.wifi === 'active' || l.carplay === 'active') && s.now_ms - l.updated_ms < 60_000);
};
/** A connection failure from the last ten minutes, still worth showing. */
export const recentLinkError = (s: State) => s.link.error && s.now_ms - s.link.error.ts_ms < 600_000 ? s.link.error : null;
/** The phone the receiver is calling (or just failed to reach, or used before a stop), else the default one. */
export function targetPhone(s: State | null): Phone | undefined {
  const list = s?.phones ?? [];
  if (s?.pairing.device && (pairingActive(s.pairing) || s.pairing.pairing_error || /^paired$/i.test(s.pairing.phase))) return list.find(x=>x.id===s.pairing.device);
  const calling = !!s && (linkBusy(s) || !!recentLinkError(s) || !!s.stopped);
  return list.find(x => x.current) ?? (calling ? list.find(x => x.selected) : undefined) ?? list.find(x => x.preferred) ?? list[0];
}
export type PairStage = 'start' | 'waiting' | 'code' | 'finishing' | 'connecting' | 'failed' | 'connected';
/** One reading of the pairing window for every page. After the bond, the window ends (Cancelled)
 * while the receiver invites the new phone; that phone stays the window's device until a new window. */
export function pairStage(s: State): PairStage {
  const p = s.pairing, dev = p.device ? s.phones?.find(x => x.id === p.device) : undefined;
  if (pairingCode(p)) return 'code';
  if (pairingFinishing(p)) return 'finishing';
  if (pairingWaiting(p)) return 'waiting';
  if (dev?.current) return 'connected';
  // A new attempt clears the link error, so an error still present is newer than any busy step.
  if (dev && !s.hub.phone && dev.selected && recentLinkError(s)) return 'failed';
  if (dev && !s.hub.phone && (/^paired$/i.test(p.phase) || (dev.selected && linkBusy(s)))) return 'connecting';
  return 'start';
}
export type StepState = 'idle' | 'active' | 'ok' | 'fail';
export interface Link {
  bluetooth: StepState; bluetooth_detail: string; wifi: StepState; wifi_detail: string; carplay: StepState; carplay_detail: string;
  error: { step: 'bluetooth' | 'wifi' | 'carplay'; title: string; hint: string; ts_ms: number } | null; updated_ms: number;
}
export interface Spec { width: number; height: number; fps: number; width_mm: number; hevc: boolean; microphone: boolean }
export interface TimelineEvent { ts_ms: number; kind: string; msg: string; detail?: string }
export interface SessionSummary { id: string; started_ms: number; ended_ms: number | null; phone: string | null; peer: string | null; first_frame_ms: number | null; stalls: number; events: TimelineEvent[] }
export interface State {
  now_ms: number; version: string; build: string; uptime_s: number; diag_code: string;
  me: { id: string; kind: 'screen' | 'phone'; name: string; prefs: ScreenPrefs; window: WindowInfo | null; viewing: boolean };
  viewer: { id: string; name: string; ua: string; ip: string; prefs: ScreenPrefs; window: WindowInfo | null } | null;
  screens: (ClientInfo & { viewing: boolean })[];
  phones: Phone[] | null; phones_error: string | null; current_phone: string | null;
  /** Someone pressed “断开 CarPlay”; screens do not call a phone until a connect/switch/pair. */
  stopped?: boolean;
  /** An iPhone is between Bluetooth and CarPlay (iAP2 up, no session yet); bounded by the service. */
  handoff?: boolean;
  pairing: PairingStatus;
  link: Link;
  hub: { verified: boolean; phone: boolean; viewer: boolean; video: boolean; display: string; protocol: string;
    ultra?: {enabled: boolean; channels: boolean[]; climate_control: {available: boolean; allowed: boolean; feedback?: {sequence:number; at_ms:number; age_ms:number; message:string}}} };
  vehicle?: {enabled: boolean; connected: boolean; phase: string};
  spec: Spec | null;
  display_limits?: DisplayLimits;
  session: SessionSummary | null;
  policy: { ask_when_multi: boolean; ask_timeout: number; stall_ms: number; retries: number; stall_card: boolean; token_required: boolean; max_fps: number; max_res: string };
  network: Network;
  wifi_qr: string | null;
  diag: { level: 'ok' | 'warn' | 'error' | 'unknown'; problems: number; title: string };
}
export interface DisplayLimits {
  receiver:{min_width:number;min_height:number;max_width:number;max_height:number;dimension_step:number;frame_rates:number[];codecs:string[];resolution_aliases:Record<string,string>};
  iphone:{ranges_known:false;note:string}; requested:Spec|null; observed:null;
}
export interface Network { device_name?: string; receiver_name?: string; platform?: string; ssid: string | null; channel: number | null; origin: string; host: string; lan_ip: string | null; luci: string | null; cert_days: number | null; cert_source: string; ddns: { enabled: boolean; provider: string; last_ms: number | null; ok: boolean | null } }
export const getState = () => api<State>('/api/state');
export const setPrefs = (prefs: Partial<ScreenPrefs>, target?: 'viewer', signal?: AbortSignal, screenId?: string) => api<{ prefs: ScreenPrefs; spec: Spec | null }>('/api/prefs', { body: {prefs,...(target?{target}:{}),...(screenId?{screen_id:screenId}:{})}, signal });

// ---------- statistics and diagnosis ----------
export interface CarReport { fps?: number; decode_ms?: number; rtt_ms?: number; jitter_ms?: number; loss_pct?: number; decoded?: number; dropped?: number; width?: number; height?: number; decoder?: string; audio_ms?: number; buffer_ms?: number; codec?: string; color?: string; audio_rate?: number; audio_channels?: number; audio_codec?: string; visible?: boolean; intervals?: number[] }
export interface Stats {
  ts_ms: number; diag_code: string; session: { id: string; started_ms: number; phone: string | null; first_frame_ms: number | null } | null; spec: Spec | null;
  server: { frames_in: number; bytes_in: number; dropped: number; keyframe_requests: number; fps_in: number; bitrate_bps: number; since_last_frame_ms: number | null; phone_rtt_ms: number | null };
  car: CarReport | null; car_last_report?: CarReport | null; car_sample_age_ms?: number | null; history: Record<string, (number | null)[]>; stalls_10m: number;
  f50: { sample_age_ms?: number | null; cpu: number | null; temp: number | null; mem_free_mb: number | null; mem_total_mb: number | null; wifi: { sample_age_ms?: number; iface: string | null; ssid: string | null; channel: number | null; width_mhz: number | null; clients: number | null } | null; uplink: string | null };
  /** model/ios: what the iPhone reported in its CarPlay SETUP (hardware identifier, iOS version). */
  phone: { ip: string | null; name: string | null; rssi: number | null; rate_mbps: number | null; retry_pct: number | null; bt: string | null; model?: string | null; ios?: string | null; wifi_status?: string; associated?: boolean | null };
}
/** iPhone Wi‑Fi link quality; says so when the station is on the hotspot but the driver reports nothing. */
export function phoneWifi(st: Stats | null): string {
  const p = st?.phone;
  if (!p) return '—';
  if (p.rssi == null && p.rate_mbps == null) {
    if (p.associated) return '已连热点';
    const reasons: Record<string,string> = {unidentified:'尚未识别热点关联',station_missing:'未找到热点关联',query_failed:'热点关联查询失败',no_interface:'未找到热点接口',disconnected:'iPhone 未连接',stale:'热点关联已过期'};
    return reasons[p.wifi_status ?? ''] ?? '关联未知';
  }
  return [p.rssi != null ? `${Math.round(p.rssi)} dBm` : null, p.rate_mbps != null ? `${Math.round(p.rate_mbps)} Mbps` : null].filter(Boolean).join(' · ');
}
/** “iPhone14,2 · iOS 18.1”, or — when the iPhone has not reported it in this session. */
export function phoneModel(st: Stats | null): string {
  const p = st?.phone;
  return p?.model || p?.ios ? [p.model, p.ios ? `iOS ${p.ios}` : null].filter(Boolean).join(' · ') : '—';
}
export interface DiagCheck { group: string; name: string; value: number | null; text: string; limit: string; ratio: number; ok: boolean | null; known: boolean }
export interface Diagnosis {
  verdict: { level: 'ok' | 'warn' | 'error' | 'unknown'; title: string; detail: string; action: { kind: 'smooth' | 'auto'; label: string } | null };
  checks: DiagCheck[]; events: { level: string; title: string; cause: string; ts_ms: number }[]; code: string; problems: number;
  selftest: { passed: number; warned: number; failed: number; checks: unknown[] } | null;
}
export interface Segments { steps: { step: string; ok: boolean; text: string }[]; passed: number; total: number; ms: number }
export const getStats = () => api<Stats>('/api/stats');
export const getDiag = () => api<Diagnosis>('/api/diag');
export const runSegments = (car_rtt_ms?: number) => api<Segments>('/api/selftest/segments', { body: car_rtt_ms === undefined ? {} : { car_rtt_ms } });
