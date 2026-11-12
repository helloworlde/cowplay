import {h, radioKeys} from './dom';
import type {DisplayLimits, ScreenPrefs, Spec, WindowInfo} from './api';
import './picture-editor.css';

const keys = ['quality','resolution','fps','codec','ui_size','right_hand_drive'] as const;
const aliases: Record<string,[number,number]> = {'720p':[1280,720],'1080p':[1920,1080],'1440p':[2560,1440],'2160p':[3840,2160]};
const exact=(v:string):[number,number]|null=>v!=='1.5x'&&v.split('x').length===2?v.split('x').map(Number) as [number,number]:null;
export function wantedPicture(p:ScreenPrefs,rw:number,rh:number,dpr:number,cap:string,maxFps:number,bounds?:DisplayLimits['receiver']):Spec|null {
 const scale=p.quality==='sharp'||(p.quality==='custom'&&p.resolution==='1.5x')?1.5:1;
 const dimensions=p.quality==='custom'?exact(p.resolution):null;
 if(dimensions&&!dimensions.every(v=>Number.isInteger(v)&&v>0&&v%2===0))return null;
 let [w,hh]=p.quality==='custom'&&exact(p.resolution)||[rw*dpr*scale,rh*dpr*scale];
 if(p.quality==='custom'&&aliases[p.resolution]){const [mw,mh]=aliases[p.resolution];const f=Math.min(mw/w,mh/hh);w*=f;hh*=f;}
 const [mw,mh]=aliases[cap]??exact(cap)??aliases['1080p'],f=Math.min(mw/w,mh/hh,1);
 const width=Math.floor(w*f/2)*2,height=Math.floor(hh*f/2)*2;
 if(bounds&&(width<bounds.min_width||height<bounds.min_height))return null;
 return {width,height,fps:Math.min(p.quality==='custom'?p.fps:p.quality==='smooth'?30:60,maxFps),hevc:p.quality==='custom'&&p.codec==='hevc',width_mm:180,microphone:p.call==='web'};
}
export interface PictureContext {
 owner:string; prefs:ScreenPrefs; window:WindowInfo|null; spec:Spec|null; limits?:DisplayLimits;
 maxRes:string; maxFps:number; busy:boolean; error:string; hevc:boolean|null|'remote';
 save(p:Partial<ScreenPrefs>):Promise<boolean>; render():void;
}
/** One draft per target screen. Polling cannot save it or move it onto a different screen. */
export class PictureEditor {
 private owner=''; private base=''; private draft:ScreenPrefs|null=null; private notice='';
 private saving=false; private generation=0; private advancedOpen=false;
 reset():void{this.owner='';this.base='';this.draft=null;this.notice='';this.saving=false;this.advancedOpen=false;this.generation++;}
 private signature(p:ScreenPrefs):string{return JSON.stringify(keys.map(k=>p[k]??false));}
 private sync(c:PictureContext):ScreenPrefs {
  const signature=this.signature(c.prefs);
  if(this.owner!==c.owner){this.reset();this.owner=c.owner;this.base=signature;}
  else if(signature!==this.base&&!this.saving){if(this.draft)this.notice='画面设置已在其他页面更新，请重新调整。';this.draft=null;this.base=signature;}
  return this.draft??c.prefs;
 }
 view(c:PictureContext,prefix:string):HTMLElement {
  const p=this.sync(c),caps=c.limits?.receiver,busy=c.busy||this.saving;
  const dirty=this.signature(p)!==this.base;
  const edit=(patch:Partial<ScreenPrefs>)=>{this.draft={...(this.draft??p),...patch};this.notice='';c.render();};
  const custom=(patch:Partial<ScreenPrefs>)=>{const current=this.draft??p;edit({quality:'custom',...(current.quality==='custom'?{}:{resolution:current.quality==='sharp'?'1.5x':'native',fps:current.quality==='smooth'?30:60,codec:'h264'}),...patch});};
  const field=(name:string,title:string,value:string,options:[string,string][],change:(v:string)=>void,disabled=false)=>h('label',{class:'picture-field'},h('span',{},title),
   h('select',{id:`${prefix}-${name}`,'aria-label':title,disabled:busy||disabled,onchange:(e:Event)=>{(e.target as HTMLSelectElement).focus({preventScroll:true});(e.target as HTMLSelectElement).blur();change((e.target as HTMLSelectElement).value);}},
    ...(!options.some(([v])=>v===value)?[h('option',{value:name==='resolution'?value:'',disabled:name!=='resolution',selected:true},name==='resolution'?`自定义 ${value.replace('x','×')}`:`原设置 ${value}（请重新选择）`)]:[]),...options.map(([v,t])=>h('option',{value:v,selected:v===value},t))));
  const options:[string,string][]=[['native','跟随屏幕'],['1.5x','屏幕 1.5 倍'],
   ...Object.entries(aliases).filter(([k])=>caps?.resolution_aliases[k]).map(([k])=>[k,`${k==='2160p'?'4K':k} · 按屏幕比例`] as [string,string]),
   ['800x480','800×480'],['1280x720','1280×720'],['1920x720','1920×720 · 超宽'],['1920x1080','1920×1080'],['720x1280','720×1280 · 竖屏'],['1080x1920','1080×1920 · 竖屏'],['custom','输入宽高…']];
  const dimensions=p.quality==='custom'?exact(p.resolution):null,r=c.window;
  const wanted=r?wantedPicture(p,r.rw,r.rh,r.dpr,c.maxRes,c.maxFps,caps):dimensions?wantedPicture(p,dimensions[0],dimensions[1],1,c.maxRes,c.maxFps,caps):c.spec;
  const valid=!!caps&&(!dimensions||(!!wanted&&dimensions[0]>=caps.min_width&&dimensions[0]<=caps.max_width&&dimensions[1]>=caps.min_height&&dimensions[1]<=caps.max_height&&dimensions.every(v=>Number.isInteger(v)&&v%caps.dimension_step===0)));
  const submit=async(patch:Partial<ScreenPrefs>)=>{
   if(c.busy||this.saving)return;const generation=this.generation;this.saving=true;c.render();
   try{if(await c.save(patch)){if(generation===this.generation){this.draft=null;this.base=this.signature({...c.prefs,...patch});this.notice='已保存画面设置。';}}}
   finally{if(generation===this.generation){this.saving=false;c.render();}}
  };
  const size=dimensions??(r?[Math.floor(r.rw*r.dpr/2)*2,Math.floor(r.rh*r.dpr/2)*2]:[1280,720]);
  const advanced=h('details',{class:'picture-advanced',open:this.advancedOpen,ontoggle:(e:Event)=>{const el=e.currentTarget as HTMLDetailsElement;if(el.isConnected)this.advancedOpen=el.open;}},
   h('summary',{id:`${prefix}-advanced`},h('span',{},'更多设置'),h('span',{class:'picture-advanced-value'},`${p.ui_size==='small'?'小':p.ui_size==='large'?'大':'标准'}控件 · ${p.right_hand_drive?'右舵':'左舵'}`)),
   h('div',{class:'picture-advanced-body'},h('h2',{},'CarPlay 布局'),h('div',{class:'picture-fields picture-layout-fields'},
    field('ui-size','CarPlay 控件大小',p.ui_size,[['small','小'],['std','标准'],['large','大']],v=>edit({ui_size:v as ScreenPrefs['ui_size']})),
    field('rhd','CarPlay 驾驶位',String(p.right_hand_drive??false),[['false','左舵'],['true','右舵']],v=>edit({right_hand_drive:v==='true'}))),
    h('div',{class:'picture-advanced-slot'}),
    h('p',{class:'picture-note'},c.hevc==='remote'?'编码能力以车机浏览器为准，实际画质以 iPhone 返回为准。':'H.265 需 iPhone 与浏览器共同支持；没有画面时可恢复兼容画质。')));
  return h('div',{id:`${prefix}-controls`,class:'picture-editor picture-controls','data-owner':c.owner},
   h('div',{class:'picture-presets',role:'radiogroup','aria-label':'画质档位',onkeydown:radioKeys},...([['auto','自动'],['smooth','流畅'],['sharp','清晰'],['custom','自定义']] as const).map(([v,t])=>h('button',{type:'button',id:`${prefix}-preset-${v}`,role:'radio',tabindex:p.quality===v?0:-1,'aria-checked':String(p.quality===v),disabled:busy,onclick:()=>edit({quality:v,...(v==='custom'?{}:{resolution:v==='sharp'?'1.5x':'native',fps:v==='smooth'?30:60,codec:'h264'})})},t))),
   h('p',{class:'picture-preset-hint'},p.quality==='auto'?'跟随屏幕尺寸，优先兼容':p.quality==='smooth'?'降低帧率，减少设备负载':p.quality==='sharp'?'以屏幕 1.5 倍尺寸请求画面':'分别设置尺寸、帧率与编码'),
   p.quality==='custom'?h('div',{class:'picture-fields'},
    field('resolution','分辨率',p.quality==='custom'?p.resolution:p.quality==='sharp'?'1.5x':'native',options,v=>custom({resolution:(v==='custom'?`${size[0]}x${size[1]}`:v) as ScreenPrefs['resolution']}),!caps),
    field('fps','帧率上限',String(p.quality==='custom'?p.fps:p.quality==='smooth'?30:60),(caps?.frame_rates??[]).filter(v=>v===30||v===60).map(v=>[String(v),`${v} 帧/秒`]),v=>custom({fps:Number(v)}),!caps),
    field('codec','视频编码',p.codec,[['h264','H.264 · 兼容'],...(c.hevc===false?[]:[['hevc','H.265 优先'] as [string,string]])],v=>custom({codec:v as ScreenPrefs['codec']}),!caps),
    ...(dimensions?[h('div',{class:'picture-dimensions'},...(['宽','高'] as const).map((title,i)=>h('label',{},title+'（px）',h('input',{type:'number',id:`${prefix}-${i?'height':'width'}`,'aria-label':title+'（px）',min:i?caps?.min_height:caps?.min_width,max:i?caps?.max_height:caps?.max_width,step:caps?.dimension_step??2,value:dimensions[i],disabled:busy,onchange:(e:Event)=>{(e.target as HTMLInputElement).blur();const next=[...dimensions];next[i]=Number((e.target as HTMLInputElement).value);custom({resolution:`${next[0]}x${next[1]}`});}}))),
     h('button',{type:'button',class:'picture-text-button',disabled:busy,onclick:()=>custom({resolution:`${dimensions[1]}x${dimensions[0]}`})},'交换宽高'))]:[])):null,
   h('div',{class:'picture-summary',role:'status'},h('span',{},dirty?'待应用画质':'请求画质'),h('strong',{},wanted?`${wanted.width}×${wanted.height} · 最高 ${wanted.fps} 帧 · ${wanted.hevc?'H.265 优先':'H.264'}`:'画面尺寸不可用，请检查宽高和设备上限')),
   dimensions&&wanted&&(wanted.width!==dimensions[0]||wanted.height!==dimensions[1])?h('p',{class:'picture-warning'},`所填 ${dimensions[0]}×${dimensions[1]} 受设备上限限制，将保持比例缩小为 ${wanted.width}×${wanted.height}。`):null,
   c.hevc===false?h('p',{class:'picture-warning'},'此浏览器未检测到 HEVC 解码支持，请使用 H.264。'):null,
   !valid?h('p',{class:'picture-error',role:'status'},'宽高必须是接收端范围内的偶数，请重新填写。'):null,
   h('p',{id:`${prefix}-error`,class:`picture-feedback ${c.error?'picture-error':'picture-note'}`,role:'status'},c.error||this.notice),
   h('div',{class:'picture-apply-row'},h('button',{type:'button',id:`${prefix}-apply`,class:'picture-apply',disabled:busy||!dirty||!valid||(p.codec==='hevc'&&p.quality==='custom'&&c.hevc===false),onclick:()=>{const current=this.draft??c.prefs;void submit(Object.fromEntries(keys.filter(k=>current[k]!==c.prefs[k]).map(k=>[k,current[k]])));}},c.busy?'正在保存…':this.saving?'等待确认…':'应用画面设置'),
    dirty?h('button',{type:'button',id:`${prefix}-undo`,class:'picture-text-button',disabled:busy,onclick:()=>{this.draft=null;this.notice='';c.render();}},'撤销修改'):null,
    h('button',{type:'button',id:`${prefix}-reset`,class:'picture-reset',title:'恢复 720p / 30 帧 / H.264',disabled:busy,onclick:()=>void submit({quality:'custom',resolution:'720p',fps:30,codec:'h264'})},'恢复兼容画质')),
   h('p',{class:'picture-note'},'应用后 iPhone 会短暂重连，配对保留。'),advanced);
 }
}

export async function probeHevc():Promise<boolean|null>{
 if(typeof VideoDecoder==='undefined')return false;
 let unknown=false;
 for(const codec of ['hvc1.1.6.L120.B0','hvc1.1.6.L93.B0','hvc1.1.6.L120.90','hvc1.2.4.L120.B0']){
  try{if((await VideoDecoder.isConfigSupported({codec,codedWidth:1280,codedHeight:720})).supported)return true;}
  catch{unknown=true;}
 }
 return unknown?null:false;
}
