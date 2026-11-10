import type {State} from './api';
import {h} from './dom';
export function displaySupport(s: State): HTMLElement {
  const c = s.display_limits, p = c?.requested;
  return h('div', {class: 'display-support', role: 'note'},
    h('p', {class: 'foot'}, c?.iphone.note ?? 'iPhone 的完整支持范围未知；以手机接受并返回的画面为准。'),
    h('p', {class: 'foot'}, p ? `向 iPhone 请求：${p.width}×${p.height} · 最高 ${p.fps} 帧 · ${p.hevc ? 'H.265' : 'H.264'}` : '尚未向 iPhone 请求画面参数。'),
    h('p', {class: 'foot'}, s.hub.video ? '已收到媒体；实际画面尺寸仍待车机解码确认。' : '尚未收到手机媒体，当前参数是否被接受仍未知。'));
}
