import {h} from './dom';
import {linkBusy,type ScreenPrefs,type State} from './api';
import './confirmation.css';

export interface Confirmation {title:string;description:string;changes?:string[];confirm:string;destructive?:boolean;current?:()=>boolean}
let active:HTMLDialogElement|null=null;
/** One native modal across all surfaces; Escape/cancel never executes an action. */
export function cancelConfirmation():void{active?.close('cancel');}
export function confirmAction(options:Confirmation):Promise<boolean>{
 if(active||document.querySelector('dialog[open]'))return Promise.resolve(false);
 const previous=document.activeElement as HTMLElement|null;
 return new Promise(resolve=>{
  const cancel=h('button',{type:'button',class:'confirmation-cancel',onclick:()=>dialog.close('cancel')},'取消');
  const accept=h('button',{type:'button',class:`confirmation-accept${options.destructive?' destructive':''}`,onclick:()=>dialog.close(options.current&&!options.current()?'cancel':'confirm')},options.confirm);
  const dialog=h('dialog',{class:'action-confirmation','aria-labelledby':'action-confirmation-title','aria-describedby':'action-confirmation-description'},
   h('h2',{id:'action-confirmation-title'},options.title),h('p',{id:'action-confirmation-description'},options.description),
   options.changes?.length?h('ul',{},...options.changes.map(value=>h('li',{},value))):null,
   h('div',{class:'confirmation-actions'},cancel,accept));
  dialog.addEventListener('keydown',event=>event.stopPropagation());dialog.addEventListener('keyup',event=>event.stopPropagation());
  const watch=options.current?window.setInterval(()=>{if(!options.current!())dialog.close('cancel');},100):0;
  dialog.addEventListener('close',()=>{clearInterval(watch);const approved=dialog.returnValue==='confirm';if(active===dialog)active=null;dialog.remove();if(previous?.isConnected)previous.focus({preventScroll:true});resolve(approved);},{once:true});
  active=dialog;document.body.append(dialog);dialog.showModal();cancel.focus({preventScroll:true});
 });
}
const names:Record<string,string>={quality:'画质方案',resolution:'分辨率',fps:'帧率上限',codec:'视频编码',ui_size:'CarPlay 控件大小',right_hand_drive:'CarPlay 驾驶位',call:'通话与 Siri'};
const labels:Record<string,string>={auto:'自动',smooth:'流畅',sharp:'清晰',custom:'自定义',native:'跟随屏幕','1.5x':'屏幕 1.5 倍','2160p':'4K',h264:'H.264',hevc:'H.265 优先',small:'小',std:'标准',large:'大',bt:'车机蓝牙',web:'这块屏幕的麦克风',true:'右舵',false:'左舵'};
export function preferenceChanges(before:ScreenPrefs,patch:Partial<ScreenPrefs>):string[]{
 return Object.keys(names).filter(key=>key in patch&&patch[key as keyof ScreenPrefs]!==before[key as keyof ScreenPrefs]).map(key=>{
  const value=String(patch[key as keyof ScreenPrefs]);return `${names[key]}：${key==='fps'?value+' 帧/秒':labels[value]??value.replace('x','×')}`;
 });
}
export const preferencePrompt=(changes:string[],remote=false):Confirmation=>({title:'应用设置并重新连接？',description:`${remote?'车机屏幕上正在投屏的':'正在投屏的'} CarPlay 会断开后重新连接，画面和声音将短暂中断，配对保留。`,changes,confirm:'应用并重新连接'});
export function commandPrompt(cmd:string,s:State,id?:string):Confirmation|null{
 const busy=!s.stopped&&linkBusy(s);
 if(cmd==='forgetPhone'){const name=s.phones?.find(p=>p.id===id)?.name??'这台 iPhone';return {title:`忘记${name}？`,description:'将删除这台 iPhone 的配对，之后需要重新配对才能使用。其他手机不受影响。',confirm:'确认忘记',destructive:true};}
 if(cmd==='openPairWindow'&&(s.hub.phone||busy||s.pairing.open||s.pairing.request!=null))return {title:s.pairing.open?'重新开始配对？':'添加新 iPhone？',description:'当前 CarPlay、正在进行的连接或配对会停止，然后打开新的配对窗口。已保存的手机配对保留。',confirm:'开始配对'};
 if(cmd==='disconnect')return {title:'断开 CarPlay？',description:'画面和声音将停止，已有配对保留。断开后不会自动重连，需要手动选择连接。',confirm:'确认断开',destructive:true};
 if(cmd==='reconnect'&&(s.hub.phone||busy))return {title:'重新连接 CarPlay？',description:'当前 CarPlay 会断开后重新连接，画面和声音将短暂中断，配对保留。',confirm:'重新连接'};
 if((cmd==='connect'||cmd==='switchPhone')&&(s.hub.phone||busy)){const phone=s.phones?.find(p=>p.id===id);return {title:phone?`连接${phone.name}？`:'切换 iPhone？',description:'当前 CarPlay 会断开，随后连接所选 iPhone。画面和声音将中断，已有配对保留。',confirm:'确认连接'};}
 if(cmd==='reloadCar'&&s.viewer)return {title:'刷新车机页面？',description:'车机页面会重新加载，画面和声音将短暂中断，iPhone 配对保留。',confirm:'刷新车机页面'};
 return null;
}

export const stateContext=(s:State|null)=>JSON.stringify(s?[s.me.id,s.viewer?.id,s.current_phone,s.session?.id,s.hub.phone,s.hub.verified,s.build,s.stopped,s.pairing.generation,!!s.pairing.request,s.pairing.open]:null);
