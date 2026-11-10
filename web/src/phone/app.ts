import type {Diagnosis, Phone, ScreenPrefs, Segments, State, Stats} from '../shared/api';
export type Page = 'status' | 'iphones' | 'diag' | 'log' | 'settings';
export const app = {
  state: null as State | null, stats: null as Stats | null, diag: null as Diagnosis | null,
  system: null as {openwrt?: string; kernel?: string; arch?: string; bluez?: string} | null,
  warnings: null as number | null,
  page: 'status' as Page, edit: false, sheet: null as 'add' | 'delete' | 'prefs' | null,
  remove: null as Phone | null, pref: 'quality' as 'quality' | 'call' | 'rail_side',
  token: null as string | null, showToken: false, tokenBusy: false, policyBusy: false, log: 'summary' as 'summary' | 'raw', level: 'all',
  events: [] as {level: string; title: string; detail: string; ts_ms: number}[],
  lines: [] as {level: string; text: string; ts_ms: number; msg: string; target: string}[],
  segments: null as Segments | null, busy: false, authorized: false, error: '', prefsBusy:false, pairCancelBusy:false, pairCancelError:'',
};
export const targetScreen = () => app.state?.viewer ?? app.state?.screens.slice().sort((a,b)=>b.last_ms-a.last_ms)[0] ?? null;
export const prefs = (): ScreenPrefs | null => targetScreen()?.prefs ?? null;
export interface Actions {
  render(): void; cmd(cmd: string, id?: string): Promise<boolean>; prefs(p: Partial<ScreenPrefs>, screenId?:string): Promise<boolean>;
  autoConnect(ask: boolean): Promise<boolean>;
  add(): void; close(): void; test(): void; export(): void; logs(): void; token(): void;
}
