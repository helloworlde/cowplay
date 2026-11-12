import type {AudioEngine} from '../audio';
import {h} from '../shared/dom';

interface Levels {master:number;muted:boolean;streams:Record<string,number>}
const key='cowplay-audio-levels';
const level=(n:unknown)=>typeof n==='number'&&Number.isFinite(n)?Math.max(0,Math.min(1,n)):1;
/** Local browser mixer: no phone reconnect and no changes to the car Bluetooth volume. */
export class AudioControls {
 private levels:Levels={master:1,muted:false,streams:{}};
 constructor(private engine:AudioEngine,private render:()=>void){
  try{const p=JSON.parse(localStorage.getItem(key)??'null');if(p&&typeof p==='object')this.levels={master:level(p.master),muted:p.muted===true,streams:Object.fromEntries(Object.entries(p.streams??{}).slice(0,16).map(([k,v])=>[k,level(v)]))};}catch{/* Storage may be unavailable; the mixer still works for this page. */}
  this.apply();
 }
 private apply(){this.engine.setVolume(this.levels.muted?0:this.levels.master);for(const [name,gain] of Object.entries(this.levels.streams))this.engine.setStreamGain(name,gain);}
 private save(){this.apply();try{localStorage.setItem(key,JSON.stringify(this.levels));}catch{}}
 view(mic:string):HTMLElement{
  const slider=(name:string,title:string,value:number,set:(n:number)=>void)=>{
   const output=h('output',{},`${Math.round(value*100)}%`);
   const input=h('input',{type:'range',min:0,max:100,step:1,value:Math.round(value*100),'aria-label':title,oninput:(e:Event)=>{const value=Number((e.target as HTMLInputElement).value)/100;set(value);output.textContent=`${Math.round(value*100)}%`;this.save();},onchange:(e:Event)=>{(e.target as HTMLInputElement).blur();this.render();}});
   return h('label',{class:'audio-slider','data-stream':name},h('span',{},title),output,input);
  };
  const labels:Record<string,string>={media:'媒体',alert:'提示音',default:'默认音频',telephony:'通话',speechRecognition:'Siri'};
  const streams=this.engine.activeStreamNames();
  return h('section',{class:'audio-mixer','aria-label':'浏览器音量'},
   slider('master','总音量',this.levels.master,n=>{this.levels.master=n;this.levels.muted=false;}),
   h('button',{type:'button',class:'audio-mute','aria-pressed':String(this.levels.muted),onclick:(e:Event)=>{this.levels.muted=!this.levels.muted;this.save();const button=e.currentTarget as HTMLButtonElement;button.setAttribute('aria-pressed',String(this.levels.muted));button.textContent=this.levels.muted?'取消静音':'静音';this.render();}},this.levels.muted?'取消静音':'静音'),
   ...streams.map(name=>slider(name,labels[name]??name,this.levels.streams[name]??1,n=>{this.levels.streams[name]=n;})),
   h('p',{class:`audio-status${streams.length?'':' audio-empty'}`,role:'status'},streams.length?(this.engine.isUnlocked?'浏览器已允许播放；实际扬声器输出需现场确认。':'点一下页面以允许浏览器播放声音。'):'尚未收到音频流。开始音乐、导航或通话后，可在这里分别调节音量。'),
   h('details',{class:'audio-notes'},h('summary',{},'声音与麦克风说明'),h('p',{class:'audio-status'},`麦克风：${mic}`),h('p',{class:'audio-status'},'音量保存在此浏览器，不触发 iPhone 重连。')));
 }
}
