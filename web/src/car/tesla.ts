import {h} from '../shared/dom';
import {ApiError, managementCall} from '../shared/api';
import {teslaManagement, type TeslaStatus} from '../shared/tesla-management';
import {svg} from './ui';
import {app} from './app';
import './tesla.css';

type Panel = 'vehicle' | 'pair' | 'controls';
export function createTeslaSettings() {
  const element=h('div',{class:'tesla-settings',id:'tesla-settings'});
  let secret='',owner='',active=false,epoch=0;
  let widget:ReturnType<typeof teslaManagement>|null=null;
  let snapshot:TeslaStatus|null=null,paint:()=>void=()=>{};
  const panel=():Panel=>location.hash.split('/')[2]==='pair'?'pair':location.hash.split('/')[2]==='controls'?'controls':'vehicle';
  const current=(generation:number)=>active&&generation===epoch&&app.authorized&&app.view==='settings'&&app.tab==='tesla'&&app.state?.me.id===owner&&(location.hash==='#settings'||/^#settings\/tesla(?:\/|$)/.test(location.hash));
  const navigate=(next:Panel)=>{location.hash=next==='vehicle'?'settings/tesla':`settings/tesla/${next}`;};
  function hide(clear=false) {
    active=false;epoch++;widget?.dispose();widget=null;snapshot=null;paint=()=>{};element.replaceChildren();
    if(clear){secret='';owner='';}
  }
  function login(message='') {
    widget?.dispose();widget=null;snapshot=null;secret='';const generation=++epoch;
    const input=h('input',{type:'password',autocomplete:'off',required:true,minlength:64,maxlength:64,'aria-label':'管理令牌'});
    const feedback=h('p',{class:'tesla-inline-feedback',role:'status'},message);
    const submit=h('button',{type:'submit',class:'tesla-primary'},'解锁 Tesla 管理');
    const form=h('form',{id:'tesla-unlock',class:'group tesla-unlock'},
      h('span',{class:'tesla-unlock-icon'},svg('vehicle',28)),h('h2',{},'Tesla 配对与控制'),
      h('p',{class:'sub-text'},'使用与 CowPlay 管理相同的独立令牌，在此完成车辆配对、连接与空调控制。'),
      h('label',{class:'manage-field'},h('span',{},'管理令牌'),input),submit,feedback);
    form.onsubmit=async event=>{
      event.preventDefault();if(!current(generation))return;const candidate=input.value.trim();input.value='';submit.disabled=true;feedback.textContent='正在解锁…';
      try{
        await managementCall('status',{},candidate);
        if(!current(generation))return;
        secret=candidate;mount();
      }catch(error){if(current(generation))feedback.textContent=error instanceof Error?error.message:'无法解锁管理';}
      finally{if(current(generation))submit.disabled=false;}
    };
    element.replaceChildren(form);
  }
  function mount() {
    const generation=++epoch;let pairDefault=false,shown:Panel|null=null;
    const call=async(method:string,args:unknown={})=>{
      if(!current(generation))throw new Error('Tesla 管理页面已离开，请返回后重试。');
      try{
        const result=await managementCall(method,args,secret);
        return result;
      }catch(error){if(current(generation)&&error instanceof ApiError&&error.status===401)login('管理令牌已失效，请重新解锁。');throw error;}
    };
    widget=teslaManagement(call,value=>{if(current(generation)){snapshot=value;paint();}});widget.element.classList.add('tesla-in-car');
    const refresh=widget.element.querySelector<HTMLButtonElement>('#tesla-refresh')!;refresh.textContent='刷新状态';
    const steps=Array.from(widget.element.querySelectorAll<HTMLElement>('.tesla-step'));
    const details=widget.element.querySelector<HTMLElement>('.tesla-details')!;
    const vin=h('span',{class:'sub-text'},'正在读取车辆…'),badge=h('span',{class:'tesla-connection'},'读取中');
    const summary=h('div',{class:'group'},h('div',{class:'row tesla-vehicle-row'},h('span',{class:'tesla-vehicle-icon'},svg('vehicle',26)),h('span',{class:'t'},h('strong',{},'Tesla 车辆'),vin),badge));
    const add=h('div',{class:'group tesla-empty'},h('h2',{},'连接你的 Tesla'),h('p',{class:'sub-text'},'添加车辆后，在这里完成配对、查看车辆读数与控制空调。'),h('button',{id:'tesla-add',type:'button',class:'tesla-primary',onclick:()=>navigate('pair')},'＋ 添加 Tesla'));
    const nav=h('div',{class:'segs wide tesla-panels',role:'tablist','aria-label':'Tesla 管理页面'},...(['vehicle','pair','controls'] as const).map(id=>h('button',{id:`tesla-panel-${id}`,type:'button',role:'tab',onclick:()=>navigate(id)},id==='vehicle'?'车辆管理':id==='pair'?'配对':'空调控制')));
    const lock=h('button',{id:'tesla-lock',type:'button',class:'tesla-lock',onclick:()=>login()},'锁定管理');
    const toolbar=h('div',{class:'tesla-toolbar'},nav,h('div',{class:'tesla-toolbar-actions'},refresh,lock));
    refresh.remove();toolbar.lastElementChild!.prepend(refresh);
    widget.element.querySelector('.tesla-head')!.remove();
    widget.element.querySelector(':scope > p.lead:not([id])')?.remove();
    widget.element.querySelector('#tesla-save')!.textContent='保存车辆';
    const back=h('button',{id:'tesla-pair-done',type:'button',class:'tesla-back',onclick:()=>navigate('vehicle')},'返回车辆管理');
    paint=()=>{
      if(!widget||!active)return;
      const current=panel();element.dataset.panel=current;
      if(shown!==current){element.closest('.s-pane')?.scrollTo(0,0);shown=current;}
      for(const button of nav.querySelectorAll('button'))button.setAttribute('aria-selected',String(button.id===`tesla-panel-${current}`));
      steps.forEach((step,index)=>{step.hidden=current==='pair'?index>1:current==='controls'?index<3:index!==2||!snapshot?.configured;});
      details.hidden=current!=='controls';summary.hidden=current==='pair'||!snapshot?.configured;add.hidden=current!=='vehicle'||!!snapshot?.configured;back.hidden=current!=='pair';
      if(snapshot){
        vin.textContent=snapshot.vin_masked??'尚未添加车辆';badge.textContent=snapshot.vehicle?.connected?'已连接':snapshot.enabled?'未连接':'已停用';badge.classList.toggle('connected',!!snapshot.vehicle?.connected);
        if(current==='pair'&&!snapshot.configured&&!pairDefault){const enabled=widget.element.querySelector<HTMLInputElement>('#tesla-enabled')!;enabled.checked=true;enabled.dispatchEvent(new Event('change',{bubbles:true}));pairDefault=true;}
        back.textContent=snapshot.phase==='pairing_succeeded'?'完成，查看车辆':'返回车辆管理';
      }
    };
    element.replaceChildren(toolbar,summary,add,widget.element,back);paint();widget.start();
  }
  return {element,show(nextOwner:string){
    if(owner!==nextOwner){hide(true);owner=nextOwner;}
    if(!active){active=true;if(secret)mount();else login();}
    paint();return element;
  },hide};
}
