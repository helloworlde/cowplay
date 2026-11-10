import {$, clear, h} from '../shared/dom';
import {recentLinkError, type ScreenPrefs} from '../shared/api';
import {app, render} from './app';

export interface PictureActions {
  setPrefs(p: Partial<ScreenPrefs>): Promise<boolean>;
  setLimits(p: {max_res?: string; max_fps?: number}): void;
}
export {wantedPicture} from '../shared/picture-editor';
import {PictureEditor} from '../shared/picture-editor';
import {displaySupport} from '../shared/display-limits';
const editor=new PictureEditor();
export const resetPictureDraft=()=>editor.reset();
export function pictureControls(a:PictureActions,prefix:string):HTMLElement {
 const s=app.state!,p=s.me.prefs,caps=s.display_limits?.receiver;
 const limits=h('section',{class:'picture-device-limits'},h('h2',{},'设备上限 · 所有屏幕共用'),
  h('div',{class:'picture-fields picture-limit-fields'},...([['max-res','设备分辨率上限',s.policy.max_res,Object.entries(caps?.resolution_aliases??{}).map(([v])=>[v,v==='2160p'?'4K':v]),(v:string)=>a.setLimits({max_res:v})],
   ['max-fps','设备帧率上限',String(s.policy.max_fps),[['30','30 帧/秒'],['60','60 帧/秒']],(v:string)=>a.setLimits({max_fps:Number(v)})]] as const).map(([id,title,value,options,change])=>h('label',{class:'picture-field'},h('span',{},title),h('select',{id:`${prefix}-${id}`,'aria-label':title,disabled:app.prefsSaving,onchange:(e:Event)=>{(e.target as HTMLSelectElement).focus({preventScroll:true});(e.target as HTMLSelectElement).blur();change((e.target as HTMLSelectElement).value);}},
 ...(!options.some(([v])=>v===value)?[h('option',{value:id==='max-fps'?'':value,disabled:id==='max-fps',selected:true},id==='max-fps'?`原设置 ${value}（请重新选择）`:value)]:[]),...options.map(([v,t])=>h('option',{value:v,selected:v===value},t)))))),
  h('p',{class:'picture-note'},'上限由所有屏幕共用，确认后生效；投屏中的 CarPlay 会重新连接。'));
 const body=editor.view({owner:s.me.id,prefs:p,window:s.me.window,spec:s.spec,limits:s.display_limits,maxRes:s.policy.max_res,maxFps:s.policy.max_fps,busy:app.prefsSaving,error:app.prefsError,hevc:app.hevcSupport,save:a.setPrefs,render},prefix);
 body.querySelector('.picture-advanced-slot')!.append(limits,displaySupport(s));return body;
}
export function openControls(): void {
  app.sheet=true;render();
  requestAnimationFrame(()=>{if(app.sheet&&app.view==='session')$('sheet-scroll').scrollTop=0;});
}
export function renderPictureSettings(a: PictureActions): void {
  const body=$('sheet-quality'),scroller=$('sheet-scroll'),scroll=scroller.scrollTop,active=document.activeElement as HTMLElement|null,id=active?.id;
  if(!body.querySelector('button:active')&&(!body.contains(active)||!active?.matches('select, summary, input'))) {
    clear(body,pictureControls(a,'sheet-picture')); scroller.scrollTop=scroll;
    if(id) document.getElementById(id)?.focus({preventScroll:true});
  }
}
export function renderControlMetrics(live: {width:number;height:number;fps:number|null;codec:string|null}): void {
  if(!app.sheet||app.view!=='session'||!app.state)return;
  const error=recentLinkError(app.state);
  const actual=app.rendered&&live.width?`当前 ${live.width}×${live.height} · ${live.fps===null?'采样中':`${Math.round(live.fps)} 帧`}`:error?`${error.title}；可恢复兼容画质后重新连接`:'等待 iPhone 返回新画面…';
  $('sheet-live').textContent=app.prefsSaving?'正在保存…':actual;
}
