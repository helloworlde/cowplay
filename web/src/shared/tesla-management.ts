import {h} from './dom';
import {confirmAction, cancelConfirmation} from './confirmation';
import './tesla-management.css';

interface Reading {value: number; received_ms: number; sample_received_ms: number; sampled_ms: number | null; sampled_time_trusted: boolean}
interface Vehicle {
  connected: boolean; phase: string;
  values: {soc_pct: number | null; range_km: number | null; tires_kpa: (number | null)[]};
  telemetry: {soc_pct: Reading | null; range_km: Reading | null; tires_kpa: (Reading | null)[]};
}
export interface TeslaStatus {
  configured: boolean; enabled: boolean; connected: boolean; phase: string;
  pairing: string; pairing_deadline_ms: number | null; vin_masked: string | null;
  key_ready: boolean; public_key_fingerprint: string | null; read_only: boolean;
  permission?: string; climate_authorized?: boolean; vehicle: Vehicle;
  ultra?: {enabled: boolean; session: number; channels: boolean[]; climate_control: {context: number; available: boolean; allowed: boolean}};
  climate?: {authorized: boolean; key_ready: boolean; public_key_fingerprint: string | null; phase: string; state: ClimateState | null; received_ms: number | null; fresh?: boolean; last_command_status: string};
}
interface ClimateState {hvac_on: boolean | null; inside_temp_celsius: number | null; outside_temp_celsius: number | null; driver_temp_celsius: number | null; passenger_temp_celsius: number | null; min_temp_celsius: number | null; max_temp_celsius: number | null; sampled_ms: number | null}
type Call = (method: string, args?: unknown) => Promise<Record<string, any>>;
const phases: Record<string, string> = {
  unconfigured: '尚未配置车辆', configuration_error: '配置或本机密钥不可用', disabled: '车辆连接已停用',
  connecting: '正在寻找并连接车辆', authorizing: '正在验证车辆授权', reading: '已连接，正在读取车辆数据',
  not_authorized: '车辆尚未授权本机公钥', authorization_failed: '车辆授权验证失败',
  pairing: '等待车内确认授权', pairing_succeeded: '车辆已确认配对，请连接并核对读数',
  pairing_failed: '配对失败，请检查车内提示后重试', pairing_timeout: '配对超时，请检查车内提示后重试', querying: '正在读取空调状态', command_pending: '正在发送空调命令', pairing_cancelled: '配对已取消',
  disconnected: '已断开，自动重连已停止', stopped: '读取已停止',
  private_bus_unavailable: '蓝牙服务不可用', ble_scan_failed: '未发现车辆或蓝牙扫描失败',
  ble_connect_failed: '车辆蓝牙连接失败', ble_gatt_failed: '车辆数据通道不可用',
  charge_read_failed: '电量与续航读取失败', tire_read_failed: '胎压读取失败',
  cleanup_failed: '数据通道释放未确认，已停止重试', read_attempts_exhausted: '读取次数已耗尽，请重试',
  charge_paused: '电量与续航读取已暂停，请重试', tires_paused: '胎压读取已暂停，请重试', reads_paused: '车辆数据读取已暂停，请重试',
};
const field = (label: string, node: HTMLElement) => h('label', {class: 'manage-field'}, h('span', {}, label), node);
const row = (label: string, node: HTMLElement) => h('div', {class: 'tesla-row'}, h('span', {}, label), node);
const number = (value: number | null | undefined, unit: string, digits = 0) => value != null && Number.isFinite(value) ? `${value.toFixed(digits)} ${unit}` : '—';
/** A reading is shown only while fresh and trusted; otherwise "—" with a short reason (never a stale value). */
function metric(value: number | null | undefined, reading: Reading | null | undefined, unit: string, connected: boolean, ttl: number, digits = 0): [string, string] {
  if (!connected) return ['—', '车辆未连接'];
  if (value != null && Number.isFinite(value) && reading?.sampled_time_trusted && (reading.sampled_ms == null || reading.sampled_ms <= Date.now() + 5000) && Math.max(Date.now() - reading.received_ms, reading.sampled_ms == null ? 0 : Date.now() - reading.sampled_ms) <= ttl) return [number(value, unit, digits), ''];
  return reading ? ['—', '已过期'] : ['—', '暂无读数'];
}
type StepChip = '' | '下一步' | '进行中' | '已连接' | '已授权';
/** Only claims what the status proves; finished steps stay unmarked rather than guessed. */
export function teslaSteps(s: TeslaStatus): StepChip[] {
  const needsPairing = ['not_authorized', 'authorization_failed', 'pairing_failed', 'pairing_timeout', 'pairing_cancelled'].includes(s.phase);
  const setup: StepChip = !s.configured || !s.key_ready ? '下一步' : '';
  const pair: StepChip = setup ? '' : s.pairing === 'pairing' ? '进行中' : needsPairing ? '下一步' : '';
  const link: StepChip = setup || pair ? '' : s.vehicle?.connected && s.phase === 'reading' ? '已连接' : s.enabled ? '下一步' : '';
  return [setup, pair, link, s.climate?.authorized ? '已授权' : '', s.ultra?.climate_control?.allowed ? '已授权' : ''];
}
export function freshnessSummary(connected: boolean, reasons: string[]): string {
  if (!connected) return '车辆未连接，读数暂不可用。';
  const stale = reasons.filter(r => r === '已过期').length, missing = reasons.filter(r => r === '暂无读数').length;
  if (stale) return `${stale} 项读数已过期或采样时间不可信，已隐藏旧值，等待车辆新数据。`;
  if (missing === reasons.length) return '已连接，尚未收到车辆读数。';
  return '电量与续航为 30 秒内采样，胎压为 2 分钟内采样。';
}

export function teslaManagement(call: Call): {element: HTMLElement; start: () => void; dispose: () => void} {
  let state: TeslaStatus | null = null, disposed = false, pending = false, mutating = false, epoch = 0, readEpoch = 0, timer = 0, dirty = false;
  const vin = h('input', {id: 'tesla-vin', type: 'text', autocomplete: 'off', autocapitalize: 'characters', spellcheck: 'false', maxlength: 17, placeholder: '输入完整的 17 位 VIN', 'aria-label': 'Tesla VIN'});
  const enabled = h('input', {id: 'tesla-enabled', type: 'checkbox', 'aria-label': '启用 Tesla 连接'});
  vin.oninput = enabled.onchange = () => { dirty = true; };
  const status = h('p', {id: 'tesla-phase', class: 'tesla-phase', role: 'status'}, '正在读取 Tesla 状态…');
  const feedback = h('p', {id: 'tesla-feedback', class: 'lead', role: 'status'});
  const savedVin = h('span', {id: 'tesla-saved-vin'}, '—');
  const key = h('span', {id: 'tesla-key'}, '—');
  const fingerprint = h('code', {id: 'tesla-fingerprint'}, '—');
  const permission = h('span', {id: 'tesla-permission'}, '未知');
  const climateStatus = h('p', {id: 'tesla-climate-status', class: 'lead', role: 'status'}, '尚未读取空调状态');
  const climateValues = h('p', {id: 'tesla-climate-values', class: 'lead'}, '当前温度：— · 目标温度：—');
  const climateRange = h('p', {id: 'tesla-climate-range', class: 'lead'}, '车辆允许的温度范围：未知');
  const climateAge = h('p', {id: 'tesla-climate-age', class: 'lead'}, '接收时间：未知 · 车辆采样时间：未知');
  const driverKey = h('span', {id: 'tesla-driver-key'}, '—');
  const driverFingerprint = h('code', {id: 'tesla-driver-fingerprint'}, '—');
  const driver = h('input', {id: 'tesla-climate-driver', type: 'number', step: 'any', 'aria-label': '驾驶位目标温度', placeholder: '先读取车辆温度'});
  const passenger = h('input', {id: 'tesla-climate-passenger', type: 'number', step: 'any', 'aria-label': '副驾目标温度', placeholder: '先读取车辆温度'});
  let temperatureDirty = false;
  let temperatureVehicle = '';
  driver.oninput = passenger.oninput = () => { temperatureDirty = true; };
  const countdown = h('p', {id: 'tesla-pairing-deadline', class: 'lead'});
  const ultra = h('p', {id: 'tesla-ultra-state', class: 'lead'}, '启用状态：未知 · 车辆通道：未知');
  const readings = ['电量', '续航', '左前胎压', '右前胎压', '左后胎压', '右后胎压'].map((label, i) => h('div', {class: 'tesla-reading'}, h('dt', {}, label), h('dd', {id: `tesla-reading-${i}`}, '—')));
  const actions: {button: HTMLButtonElement; available: () => boolean}[] = [];
  const context = () => JSON.stringify([state?.vin_masked, state?.public_key_fingerprint, state?.enabled, state?.climate?.authorized, state?.ultra?.session, state?.ultra?.climate_control?.context, state?.ultra?.climate_control?.allowed]);
  const busy = () => state?.pairing === 'pairing' || ['pairing', 'querying', 'command_pending'].includes(state?.climate?.phase ?? '');
  const configured = () => !!state?.configured && !!state.enabled && !!state.key_ready && !busy();
  const climateFresh = () => !!state?.climate?.fresh && state.climate.received_ms != null && state.climate.state?.sampled_ms != null && Math.max(Date.now() - state.climate.received_ms, Date.now() - state.climate.state.sampled_ms) <= 30000 && state.climate.state.sampled_ms <= Date.now() + 5000;
  const sync = () => {
    for (const {button, available} of actions) button.disabled = pending || disposed || !available();
    vin.disabled = enabled.disabled = pending || !state;
    driver.disabled = passenger.disabled = pending || !state?.climate?.authorized;
  };
  function render(): void {
    if (!state || disposed) return;
    const vehicleContext = JSON.stringify([state.vin_masked, state.public_key_fingerprint, state.climate?.public_key_fingerprint]);
    if (temperatureVehicle !== vehicleContext) {
      temperatureVehicle = vehicleContext;
      temperatureDirty = false;
      driver.value = passenger.value = '';
    }
    status.textContent = phases[state.phase] ?? `车辆状态：${state.phase || '未知'}`;
    savedVin.textContent = state.vin_masked ?? '未配置';
    key.textContent = state.key_ready ? '本机密钥已就绪（保留在主机）' : '尚未准备';
    fingerprint.textContent = state.public_key_fingerprint ?? '—';
    permission.textContent = 'Vehicle Monitor · 只读车辆数据';
    countdown.textContent = state.pairing === 'pairing' ? `请在车内确认本机公钥${state.pairing_deadline_ms ? `，窗口剩余 ${Math.max(0, Math.ceil((state.pairing_deadline_ms - Date.now()) / 1000))} 秒` : ''}。` : '';
    if (!dirty) enabled.checked = state.enabled;
    const vehicle = state.vehicle;
    const values = vehicle?.values, telemetry = vehicle?.telemetry;
    const texts = [metric(values?.soc_pct, telemetry?.soc_pct, '%', !!vehicle?.connected, 30000), metric(values?.range_km, telemetry?.range_km, 'km', !!vehicle?.connected, 30000), ...[0, 1, 2, 3].map(i => metric(values?.tires_kpa?.[i], telemetry?.tires_kpa?.[i], 'kPa', !!vehicle?.connected, 120000))];
    readings.forEach((node, i) => { const [value, reason] = texts[i]; node.querySelector('dd')!.replaceChildren(value, reason ? h('small', {}, reason) : ''); });
    freshness.textContent = freshnessSummary(!!vehicle?.connected, texts.map(([, reason]) => reason));
    teslaSteps(state).forEach((chip, i) => { chips[i].textContent = chip; chips[i].hidden = !chip; });
    const climate = state.climate, data = climate?.state;
    ultra.textContent = `启用状态：${typeof state.ultra?.enabled === 'boolean' ? state.ultra.enabled ? '已启用' : '已停用' : '未知'} · 车辆通道：${state.ultra?.channels?.map((open, i) => `${i === 0 ? '高优先级' : '标准'}${open ? '已建立' : '未建立'}`).join(' / ') ?? '未知'} · 原生空调：${state.ultra?.climate_control?.allowed ? '已允许本次 CarPlay 控制' : '未授予本次 CarPlay 控制权限'} · ${state.ultra?.climate_control?.available ? '车辆读数有效' : '请先读取车辆空调状态'}`;
    driverKey.textContent = climate?.key_ready ? climate.authorized ? '控制身份已通过车辆认证，空调权限以命令回执为准' : '本机 Driver 身份已就绪，车辆授权待验证' : '尚未准备独立 Driver 身份';
    driverFingerprint.textContent = climate?.public_key_fingerprint ?? '—';
    const current = data && climateFresh();
    climateValues.textContent = current ? `空调：${data.hvac_on == null ? '未知' : data.hvac_on ? '开启' : '关闭'} · 车内 ${number(data.inside_temp_celsius, '°C', 1)} · 车外 ${number(data.outside_temp_celsius, '°C', 1)} · 目标：驾驶位 ${number(data.driver_temp_celsius, '°C', 1)} / 副驾 ${number(data.passenger_temp_celsius, '°C', 1)}` : '空调与温度：— · 尚无当前读数或读数已过期';
    const commands: Record<string, string> = {idle: '尚未发送控制命令', pending: '正在发送单次命令…', confirmed: '车辆已确认命令；当前读数以上次读取为准', unconfirmed: '命令结果未确认，请读取状态后判断，不会自动重试', cancelled: '命令已取消，车辆结果需重新读取'};
    climateStatus.textContent = climate?.phase === 'querying' ? '正在连接车辆并读取空调状态…' : climate?.phase === 'pairing' ? '正在申请 Driver 授权，请在车内确认…' : climate?.phase === 'command_pending' ? commands.pending : climate?.last_command_status === 'unconfirmed' ? commands.unconfirmed : climate?.phase === 'failed' ? '空调任务失败，请检查车辆连接与授权后重试。' : climate?.phase === 'cancelled' ? '空调任务已取消；已发送命令的结果仍需重新读取。' : climate?.authorized ? commands[climate.last_command_status] ?? '空调状态未知' : '空调需要显式升级车辆控制授权；现有只读授权不足，可读取状态验证本机已有的 Driver 授权。';
    climateAge.textContent = `接收时间：${climate?.received_ms != null ? `${Math.max(0, Math.floor((Date.now() - climate.received_ms) / 1000))} 秒前` : '未知'} · 车辆采样时间：${data?.sampled_ms != null ? `${Math.max(0, Math.floor((Date.now() - data.sampled_ms) / 1000))} 秒前` : '未知'}`;
    climateRange.textContent = `车辆允许的温度范围：${current && data.min_temp_celsius != null && data.max_temp_celsius != null ? `${data.min_temp_celsius}–${data.max_temp_celsius} °C` : '未知，请读取车辆状态'}`;
    if (!temperatureDirty && current) { driver.value = data.driver_temp_celsius == null ? '' : String(data.driver_temp_celsius); passenger.value = data.passenger_temp_celsius == null ? '' : String(data.passenger_temp_celsius); }
    for (const input of [driver, passenger]) { if (current && data.min_temp_celsius != null) input.min = String(data.min_temp_celsius); else input.removeAttribute('min'); if (current && data.max_temp_celsius != null) input.max = String(data.max_temp_celsius); else input.removeAttribute('max'); }
    sync();
  }
  async function refresh(): Promise<void> {
    if (disposed) return;
    render();
    if (mutating) return;
    const current = epoch, read = ++readEpoch;
    try {
      const [result] = await Promise.all([call('tesla_status'), call('status').catch((): Record<string, any> => ({}))]);
      if (disposed || current !== epoch || read !== readEpoch) return;
      if (typeof result.configured !== 'boolean' || !result.vehicle) throw Error('Tesla 状态不完整，请核对核心版本。');
      state = result as TeslaStatus; render();

    } catch (error) {
      if (!disposed && current === epoch && read === readEpoch) { status.textContent = '无法读取 Tesla 当前状态'; feedback.textContent = error instanceof Error ? error.message : '状态请求失败'; }
    }
  }
  function action(id: string, label: string, method: string, prompt: {title: string; description: string; confirm: string} | null, available: () => boolean, args: () => unknown = () => ({})): HTMLButtonElement {
    const button = h('button', {id, type: 'button', class: 'primary'}, label);
    actions.push({button, available});
    button.onclick = async () => {
      if (pending || disposed || !available()) return;
      const before = context(), draft = JSON.stringify([vin.value, enabled.checked, driver.value, passenger.value]);
      const current = () => !disposed && available() && before === context() && draft === JSON.stringify([vin.value, enabled.checked, driver.value, passenger.value]);
      pending = true; sync();
      try {
        const values = args();
        const changes = method === 'tesla_climate_command' && (values as {action: string}).action === 'temperature' ? [`驾驶位：${driver.value} °C`, `副驾：${passenger.value} °C`] : undefined;
        if (prompt && !await confirmAction({...prompt, changes, current}) || !current()) return;
        const request = ++epoch;
        mutating = true;
        feedback.textContent = '正在发送请求…';
        const result = await call(method, values);
        if (disposed || request !== epoch) return;
        if (typeof result.configured !== 'boolean' || !result.vehicle) throw Error('未收到完整操作状态，请刷新核对后再重试。');
        state = result as TeslaStatus;
        if (method === 'tesla_configure') { vin.value = ''; dirty = false; }
        feedback.textContent = method === 'tesla_configure' ? '配置已保存。本机密钥由主机生成或复用；授权和连接结果见实时状态。' : method === 'tesla_climate_command' ? '已返回单次命令结果，请查看车辆确认状态；页面不会推测车辆温度。' : '请求已返回，结果以车辆实时状态为准。';
        render();
      } catch (error) {
        if (!disposed) feedback.textContent = `操作结果未确认：${error instanceof Error ? error.message : '请求失败'}。请核对实时状态后重试。`;
      } finally { mutating = false; if (!disposed) { pending = false; sync(); } }
    };
    return button;
  }
  const save = action('tesla-save', '保存 VIN 并准备本机密钥', 'tesla_configure', {title: '保存 Tesla 配置？', description: '将保存车辆 VIN，并在主机生成或复用本机密钥。已有车辆连接会重新读取配置；私钥保留在主机。', confirm: '保存配置'}, () => !!state && !busy(), () => {
    const value = vin.value.trim().toUpperCase();
    if (value && !/^[A-HJ-NPR-Z0-9]{17}$/.test(value) || !value && !state?.configured) throw Error('请输入 17 位 VIN，不包含 I、O、Q');
    return {...(value ? {vin: value} : {}), enabled: enabled.checked};
  });
  const pair = action('tesla-pair', '开始车内配对', 'tesla_pair', {title: '开始 Tesla 车内授权？', description: '请将车辆停稳，在车内按车辆提示确认本机公钥。当前申请 Vehicle Monitor 只读车辆数据授权，不代表空调控制权限。配对窗口有时间限制，可以取消。', confirm: '开始配对'}, configured);
  const cancel = action('tesla-cancel', '取消当前任务', 'tesla_cancel', {title: '取消当前 Tesla 任务？', description: '将关闭本次配对或空调任务并清理连接。已有车辆授权和本机密钥保留；按原连接设置恢复只读数据监测。已发送命令的车辆结果仍需重新读取。', confirm: '取消当前任务'}, busy);
  const connect = action('tesla-connect', '连接 / 重试', 'tesla_connect', {title: '连接 Tesla？', description: '将尝试连接已配置车辆并验证本机公钥授权。连接与读取结果见实时状态，失败后可手动重试。', confirm: '连接车辆'}, configured);
  const disconnect = action('tesla-disconnect', '断开车辆', 'tesla_disconnect', {title: '断开 Tesla 连接？', description: '将停止车辆数据读取、当前任务和自动重连，已有车辆授权与本机密钥保留。之后可手动连接。', confirm: '断开车辆'}, () => !!state?.configured);
  const climatePair = action('tesla-climate-pair', '升级车辆控制授权', 'tesla_climate_pair', {title: '申请 Driver 车辆控制授权？', description: '现有 Vehicle Monitor 授权只读。此次需在车内确认 Driver 角色，车辆授予的能力范围广于空调；CowPlay 页面仅发送空调开关与双区目标温度。不会自动升级授权。', confirm: '申请并在车内确认'}, configured);
  const climateQuery = action('tesla-climate-query', '读取空调状态', 'tesla_climate_query', null, () => configured() && !!state?.climate?.key_ready);
  const climatePower = (on: boolean) => action(`tesla-climate-${on ? 'on' : 'off'}`, on ? '开启空调' : '关闭空调', 'tesla_climate_command', {title: `${on ? '开启' : '关闭'}车辆空调？`, description: '将向当前车辆发送一次空调命令。车辆确认与当前读数分别显示；结果未确认时不会自动重试。', confirm: on ? '开启空调' : '关闭空调'}, () => configured() && !!state?.climate?.authorized, () => ({action: 'power', on}));
  const setTemperature = action('tesla-climate-temperature', '设置双区目标温度', 'tesla_climate_command', {title: '设置车辆目标温度？', description: '将向当前车辆发送一次驾驶位与副驾目标温度命令，以表单中两个数值为准。车辆确认不会覆盖当前读数，结果未确认时不会自动重试。', confirm: '设置目标温度'}, () => configured() && !!state?.climate?.authorized && climateFresh() && !!state.climate.state && state.climate.state.min_temp_celsius != null && state.climate.state.max_temp_celsius != null, () => {
    const low = state?.climate?.state?.min_temp_celsius, high = state?.climate?.state?.max_temp_celsius;
    const left = Number(driver.value), right = Number(passenger.value);
    if (!driver.value || !passenger.value || !Number.isFinite(left) || !Number.isFinite(right) || low == null || high == null || left < low || left > high || right < low || right > high) throw Error('请按车辆返回的温度范围填写两个目标温度');
    return {action: 'temperature', driver_celsius: left, passenger_celsius: right};
  });
  const reload = h('button', {type: 'button', id: 'tesla-refresh', class: 'secondary', onclick: () => { void refresh(); }}, '刷新状态');
  const nativeGrant = action('tesla-ultra-allow', '允许本次 CarPlay 控制空调', 'tesla_ultra_control', {title: '允许当前 iPhone 控制车辆空调？', description: '允许当前已认证 CarPlay 会话发送空调开关和双区温度命令。不会申请新的车辆权限；换手机、重连、换车或停服后需重新允许，可随时撤销。', confirm: '允许本次会话'}, () => configured() && climateFresh() && !!state?.ultra?.climate_control?.available && state.ultra.channels.some(Boolean) && !state.ultra.climate_control.allowed, () => ({enabled: true, session: state!.ultra!.session, context: state!.ultra!.climate_control.context}));
  const nativeRevoke = action('tesla-ultra-revoke', '撤销本次 CarPlay 空调控制', 'tesla_ultra_control', null, () => !!state?.ultra?.climate_control?.allowed, () => ({enabled: false, session: state!.ultra!.session, context: state!.ultra!.climate_control.context}));
  const climateOn = climatePower(true), climateOff = climatePower(false);
  // One primary action per step; the rest are secondary or destructive.
  for (const [button, role] of [[cancel, 'secondary'], [disconnect, 'destructive'], [climatePair, 'secondary'], [climateOn, 'secondary'], [climateOff, 'secondary'], [setTemperature, 'secondary'], [nativeRevoke, 'destructive']] as const) button.className = `action ${role}`;
  const chips = [0, 1, 2, 3, 4].map(() => h('span', {class: 'tesla-chip', hidden: true}));
  const step = (n: number, title: string, ...children: (Node | null)[]) => h('section', {class: 'tesla-step', 'aria-label': title},
    h('h3', {}, h('span', {class: 'tesla-step-no', 'aria-hidden': 'true'}, String(n)), h('span', {class: 'tesla-step-title'}, title), chips[n - 1]), ...children);
  const element = h('section', {id: 'tesla-management', class: 'tesla-management', 'aria-labelledby': 'tesla-title'},
    h('div', {class: 'tesla-head'}, h('h2', {id: 'tesla-title'}, 'Tesla 车辆'), reload), h('p', {class: 'lead'}, '按步骤完成配置、车内配对和连接。离开此页后车辆连接保留。'), status, feedback,
    step(1, '车辆', row('已保存的车辆', savedVin),
      h('form', {id: 'tesla-config-form'}, field('车辆 VIN', vin), h('p', {class: 'lead'}, '已有 VIN 不会回填。首次或更换车辆时输入完整 VIN；留空会保留已保存车辆。'), field('启用 Tesla 连接', enabled), save)),
    step(2, '车内配对', h('p', {class: 'lead'}, '车辆停稳后开始，在车内屏幕确认本机公钥。只申请读取车辆数据的权限；失败不会清除已有授权。'), countdown, h('div', {class: 'tesla-actions'}, pair, cancel)),
    step(3, '连接与读数', h('div', {class: 'tesla-actions'}, connect, disconnect), h('p', {id: 'tesla-freshness', class: 'tesla-freshness', role: 'status'}), h('dl', {class: 'tesla-readings'}, ...readings)),
    step(4, '空调控制（可选）', climateStatus, climateValues, climateAge, h('div', {class: 'tesla-actions'}, climateQuery, climatePair),
      h('p', {class: 'lead'}, '升级授权需在车内确认，车辆授予的权限范围大于空调；页面只发送空调开关与双区温度，每条命令只发一次。'),
      h('div', {class: 'tesla-actions'}, climateOn, climateOff), climateRange, field('驾驶位目标温度（°C）', driver), field('副驾目标温度（°C）', passenger), setTemperature),
    step(5, 'CarPlay 原生空调（实验）', ultra, h('div', {class: 'tesla-actions'}, nativeGrant, nativeRevoke), h('p', {class: 'lead'}, '需先完成第 4 步并读取空调状态，再允许当前 CarPlay 会话控制。换手机、重连或停服后需重新允许；真实车辆效果需现场验收。')),
    h('details', {class: 'tesla-details'}, h('summary', {}, '技术详情'), row('只读身份密钥', key), row('只读公钥指纹', fingerprint), row('只读车辆授权', permission), row('独立控制身份', driverKey), row('控制公钥指纹', driverFingerprint)));
  const freshness = element.querySelector<HTMLElement>('#tesla-freshness')!;
  element.querySelector('form')!.onsubmit = event => { event.preventDefault(); save.click(); };
  sync();
  const tick = async () => { await refresh(); if (!disposed) timer = window.setTimeout(tick, 2000); };
  return {element, start: () => { if (!timer && !disposed) void tick(); }, dispose: () => { disposed = true; epoch++; clearTimeout(timer); cancelConfirmation(); }};
}
