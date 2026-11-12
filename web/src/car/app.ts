// Shared car-page state. Rendering modules read it; main.ts owns transitions.
import {targetPhone, type Diagnosis, type Phone, type Segments, type State, type Stats} from '../shared/api';

export type View = 'connect' | 'session' | 'settings';
export type Tab = 'pic' | 'audio' | 'display' | 'phones' | 'devices' | 'diag' | 'about';
/** window: waiting for a new iPhone · pair: compare digits · finishing: bond being saved. */
export type Scene = 'token' | 'onboard' | 'window' | 'pair' | 'finishing' | 'connecting' | 'choose' | 'takeover' | 'notfound' | 'stopped';

export const app = {
  state: null as State | null,
  stats: null as Stats | null,
  statsAt: 0,
  statsError: '',
  diag: null as Diagnosis | null,
  segments: null as Segments | null,
  segmentsRunning: false,
  system: null as any,
  authorized: false,
  view: 'connect' as View,
  tab: 'pic' as Tab,
  devTab: 'car' as 'car' | 'f50' | 'phone',
  /** The viewer channel is up (hello received). */
  channel: false,
  channelError: null as string | null,
  buildMismatch: false,
  stateError: null as string | null,
  /** A frame was painted in the current connection. */
  rendered: false,
  everRendered: false,
  /** The user disconnected CarPlay from this screen; no automatic calls until they connect again. */
  stopped: false,
  /** Choose list: automatic (several online) or opened by "换一台 iPhone". */
  choose: null as null | {manual: boolean; picked: string | null; deadline: number},
  chooseDone: false,
  /** Another screen shows CarPlay; countdown deadline or null when the user kept it there. */
  takeover: null as null | {deadline: number | null},
  /** “添加 iPhone” was chosen on this screen: stay on the pairing scene until it ends or is left. */
  addPhone: false,
  /** Pairing generation when “添加” was tapped: only a session after a newer window ends the add flow. */
  addFrom: 0,
  /** Opening the pairing window failed: back off and show why. */
  openError: null as null | {message: string; at: number},
  connectingSince: 0,
  lastSearch: 0,
  setupRequired: false,
  accessOpen: false,
  /** Debug overlay is open (persisted per screen as prefs.debug). */
  fullscreen: false,
  sheet: false,
  prefsSaving: false,
  prefsError: '',
  hevcSupport: null as boolean|null,
  stall: null as null | {since: number; attempts: number; lastTry: number; cause: string},
  local: {rttMs: null as number | null, jitterMs: null as number | null, lossPct: null as number | null},
};

export function phonesOf(s: State | null): Phone[] { return s?.phones ?? []; }
export function phoneName(s: State | null): string { return targetPhone(s)?.name ?? s?.pairing.name ?? (s?.pairing.device ? '新 iPhone' : 'iPhone'); }
type Listener = () => void;
const listeners: Listener[] = [];
let queued = false;
export function onRender(fn: Listener): void { listeners.push(fn); }
/** Coalesce renders into one animation frame. */
export function render(): void {
  if (queued) return;
  queued = true;
  requestAnimationFrame(() => { queued = false; for (const fn of listeners) fn(); });
}
