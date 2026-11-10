import {type DiagCheck} from './api';
import {h} from './dom';
import './diagnostics.css';

const sources: Record<string, string> = {
  '车机 ⟷ F50': '车机浏览器的往返探测上报',
  'F50 ⟷ iPhone': 'F50 无线驱动采样',
  '画面': '浏览器绘制与解码上报；停住次数统计最近 10 分钟',
  'F50': '设备 CPU 与温度采样',
};

/** Render observations without inferring health or suggesting a configuration change. */
export function diagnosticReadings(rows: readonly (readonly [string, string])[]): HTMLElement {
  return h('dl', {class: 'measurement-values'}, ...rows.map(([name, value]) =>
    h('div', {class: 'measurement-row'}, h('dt', {}, name), h('dd', {}, value))));
}

/** Car and phone use the same grouping, units, missing-data rule and layout. */
export function diagnosticMeasurements(checks: DiagCheck[]): HTMLElement {
  const groups = [...new Set(checks.map(c => c.group))];
  return h('div', {class: 'diagnostic-measurements'}, ...groups.map(group =>
    h('section', {class: 'group measurement-group'}, h('h2', {}, group),
      h('p', {class: 'measurement-source'}, sources[group] ?? '服务采样'),
      diagnosticReadings(checks.filter(c => c.group === group).map(c => [
        c.name === '延迟' ? '往返时间 RTT' : c.name === '停住' ? '停住次数（10 分钟）' : c.name,
        c.known && c.value != null && Number.isFinite(c.value) ? c.text : '—',
      ])))));
}
