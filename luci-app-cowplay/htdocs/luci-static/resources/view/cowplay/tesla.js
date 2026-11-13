'use strict';
'require view';
'require poll';
'require cowplay.rpc as cp';
'require cowplay.confirmation as confirmation';

var phases = {unconfigured:'尚未配置车辆',configuration_error:'配置或本机密钥不可用',disabled:'车辆连接已停用',connecting:'正在寻找并连接车辆',authorizing:'正在验证车辆授权',reading:'已连接，正在读取车辆数据',not_authorized:'车辆尚未授权本机公钥',authorization_failed:'车辆授权验证失败',pairing:'等待车内确认授权',pairing_succeeded:'车辆已确认配对，请连接并核对读数',pairing_failed:'配对失败，请检查车内提示后重试',pairing_timeout:'配对超时，请检查车内提示后重试',querying:'正在读取空调状态',command_pending:'正在发送空调命令',pairing_cancelled:'配对已取消',disconnected:'已断开，自动重连已停止',stopped:'读取已停止',private_bus_unavailable:'蓝牙服务不可用',ble_scan_failed:'未发现车辆或蓝牙扫描失败',ble_connect_failed:'车辆蓝牙连接失败',ble_gatt_failed:'车辆数据通道不可用',charge_read_failed:'电量与续航读取失败',tire_read_failed:'胎压读取失败',cleanup_failed:'数据通道释放未确认，已停止重试',read_attempts_exhausted:'读取次数已耗尽，请重试',charge_paused:'电量与续航读取已暂停，请重试',tires_paused:'胎压读取已暂停，请重试',reads_paused:'车辆数据读取已暂停，请重试'};
function note(text) { return E('p', {class:'cw-foot'}, text); }
function row(label, value) { return E('div', {class:'cw-row'}, [E('span', {class:'cw-key'}, label), value]); }
function field(label, input) { return E('label', {class:'cw-tesla-field'}, [E('span', {}, label), input]); }
function numeric(value, unit) { return value != null && Number.isFinite(value) ? value.toFixed(unit === '°C' ? 1 : 0) + ' ' + unit : '—'; }
function metric(value, reading, unit, connected, ttl) {
    if (!connected) return ['—', '车辆未连接'];
    if (value != null && Number.isFinite(value) && reading && reading.sampled_time_trusted && (reading.sampled_ms == null || reading.sampled_ms <= Date.now() + 5000) && Math.max(Date.now()-reading.received_ms, reading.sampled_ms == null ? 0 : Date.now()-reading.sampled_ms) <= ttl) return [numeric(value,unit), ''];
    return reading ? ['—', '已过期'] : ['—', '暂无读数'];
}
// Same step chips and freshness wording as web/src/shared/tesla-management.ts.
function steps(s) {
    var needsPairing=['not_authorized','authorization_failed','pairing_failed','pairing_timeout','pairing_cancelled'].indexOf(s.phase)>=0, v=s.vehicle||{};
    var setup=!s.configured||!s.key_ready?'下一步':'', pair=setup?'':s.pairing==='pairing'?'进行中':needsPairing?'下一步':'';
    var link=setup||pair?'':v.connected&&s.phase==='reading'?'已连接':s.enabled?'下一步':'';
    return [setup,pair,link,(s.climate||{}).authorized?'已授权':'',((s.ultra||{}).climate_control||{}).allowed?'已授权':''];
}
function freshnessSummary(connected, reasons) {
    if (!connected) return '车辆未连接，读数暂不可用。';
    var stale=reasons.filter(function(r){return r==='已过期';}).length, missing=reasons.filter(function(r){return r==='暂无读数';}).length;
    if (stale) return stale+' 项读数已过期或采样时间不可信，已隐藏旧值，等待车辆新数据。';
    if (missing===reasons.length) return '已连接，尚未收到车辆读数。';
    return '电量与续航为 30 秒内采样，胎压为 2 分钟内采样。';
}
return view.extend({
    load: function() { return Promise.all([cp.call('tesla_status').catch(function(e) { return {error:e.message}; }),cp.call('status').catch(function() { return {}; })]); },
    context: function() { var s=this.data||{};return JSON.stringify([s.vin_masked,s.public_key_fingerprint,s.enabled,(s.climate||{}).authorized,(s.ultra||{}).session,((s.ultra||{}).climate_control||{}).context,((s.ultra||{}).climate_control||{}).allowed]); },
    busy: function() { return this.data.pairing==='pairing'||['pairing','querying','command_pending'].indexOf((this.data.climate||{}).phase)>=0; },
    climateFresh: function() { var climate=this.data.climate||{},data=climate.state;return !!climate.fresh&&climate.received_ms!=null&&data&&data.sampled_ms!=null&&Math.max(Date.now()-climate.received_ms,Date.now()-data.sampled_ms)<=30000&&data.sampled_ms<=Date.now()+5000; },
    dispose: function() { this.disposed=true;this.epoch++;poll.remove(this.pollTesla);confirmation.cancel('tesla'); },
    sync: function() {
        var self=this;
        this.buttons.forEach(function(v) { v.button.disabled=self.pending||self.disposed||!L.hasViewPermission()||!v.available(); });
        this.vin.disabled=this.enabled.disabled=this.pending||!L.hasViewPermission()||typeof this.data.configured!=='boolean'||!this.data.vehicle;
        this.driver.disabled=this.passenger.disabled=this.pending||!L.hasViewPermission()||!(this.data.climate||{}).authorized;
    },
    update: function() {
        var s=this.data, v=s.vehicle||{}, values=v.values||{}, telemetry=v.telemetry||{}, self=this;
        if (!s.vehicle) { this.phase.textContent='无法读取 Tesla 当前状态';this.feedback.textContent=s.error||'Tesla 状态不完整，请核对核心版本。';this.sync();return; }
        var vehicleContext=JSON.stringify([s.vin_masked,s.public_key_fingerprint,(s.climate||{}).public_key_fingerprint]);
        if(this.temperatureVehicle!==vehicleContext){this.temperatureVehicle=vehicleContext;this.temperatureDirty=false;this.driver.value=this.passenger.value='';}
        this.phase.textContent=phases[s.phase]||'车辆状态：'+(s.phase||'未知');
        this.savedVin.textContent=s.vin_masked||'未配置';this.key.textContent=s.key_ready?'本机密钥已就绪（保留在主机）':'尚未准备';this.fingerprint.textContent=s.public_key_fingerprint||'—';
        this.permission.textContent='Vehicle Monitor · 只读车辆数据';
        this.deadline.textContent=s.pairing==='pairing'?'请在车内确认本机公钥'+(s.pairing_deadline_ms?'，窗口剩余 '+Math.max(0,Math.ceil((s.pairing_deadline_ms-Date.now())/1000))+' 秒':'')+'。':'';
        if (!this.dirty) this.enabled.checked=s.enabled;
        var texts=[metric(values.soc_pct,telemetry.soc_pct,'%',v.connected,30000),metric(values.range_km,telemetry.range_km,'km',v.connected,30000)].concat([0,1,2,3].map(function(i) { return metric((values.tires_kpa||[])[i],(telemetry.tires_kpa||[])[i],'kPa',v.connected,120000); }));
        this.readings.forEach(function(node,i) { node.replaceChildren(texts[i][0]); if (texts[i][1]) node.appendChild(E('small',{},texts[i][1])); });this.freshness.textContent=freshnessSummary(!!v.connected,texts.map(function(t){return t[1];}));steps(s).forEach(function(chip,i){self.chips[i].textContent=chip;self.chips[i].hidden=!chip;});
        var climate=s.climate||{}, data=climate.state, current=data&&this.climateFresh();this.driverKey.textContent=climate.key_ready?climate.authorized?'控制身份已通过车辆认证，空调权限以命令回执为准':'本机 Driver 身份已就绪，车辆授权待验证':'尚未准备独立 Driver 身份';this.driverFingerprint.textContent=climate.public_key_fingerprint||'—';
        this.climateValues.textContent=current?'空调：'+(data.hvac_on==null?'未知':data.hvac_on?'开启':'关闭')+' · 车内 '+numeric(data.inside_temp_celsius,'°C')+' · 车外 '+numeric(data.outside_temp_celsius,'°C')+' · 目标：驾驶位 '+numeric(data.driver_temp_celsius,'°C')+' / 副驾 '+numeric(data.passenger_temp_celsius,'°C'):'空调与温度：— · 尚无当前读数或读数已过期';
        var commands={idle:'尚未发送控制命令',pending:'正在发送单次命令…',confirmed:'车辆已确认命令；当前读数以上次读取为准',unconfirmed:'命令结果未确认，请读取状态后判断，不会自动重试',cancelled:'命令已取消，车辆结果需重新读取'};
        this.climateStatus.textContent=climate.phase==='querying'?'正在连接车辆并读取空调状态…':climate.phase==='pairing'?'正在申请 Driver 授权，请在车内确认…':climate.phase==='command_pending'?commands.pending:climate.last_command_status==='unconfirmed'?commands.unconfirmed:climate.phase==='failed'?'空调任务失败，请检查车辆连接与授权后重试。':climate.phase==='cancelled'?'空调任务已取消；已发送命令的结果仍需重新读取。':climate.authorized?(commands[climate.last_command_status]||'空调状态未知'):'空调需要显式升级车辆控制授权；现有只读授权不足，可读取状态验证本机已有的 Driver 授权。';
        this.climateAge.textContent='接收时间：'+(climate.received_ms!=null?Math.max(0,Math.floor((Date.now()-climate.received_ms)/1000))+' 秒前':'未知')+' · 车辆采样时间：'+(data&&data.sampled_ms!=null?Math.max(0,Math.floor((Date.now()-data.sampled_ms)/1000))+' 秒前':'未知');
        this.climateRange.textContent='车辆允许的温度范围：'+(current&&data.min_temp_celsius!=null&&data.max_temp_celsius!=null?data.min_temp_celsius+'–'+data.max_temp_celsius+' °C':'未知，请读取车辆状态');
        if (!this.temperatureDirty&&current) { this.driver.value=data.driver_temp_celsius==null?'':String(data.driver_temp_celsius);this.passenger.value=data.passenger_temp_celsius==null?'':String(data.passenger_temp_celsius); }
        [this.driver,this.passenger].forEach(function(input) { ['min','max'].forEach(function(bound) { var value=current&&data[bound+'_temp_celsius'];if(current&&value!=null)input.setAttribute(bound,value);else input.removeAttribute(bound); }); });
        var ultra=this.data.ultra||((this.host||{}).hub||{}).ultra||{};
        this.ultra.textContent='启用状态：'+(typeof ultra.enabled==='boolean'?ultra.enabled?'已启用':'已停用':'未知')+' · 车辆通道：'+(Array.isArray(ultra.channels)&&ultra.channels.length===2?ultra.channels.map(function(open,i) { return (i===0?'高优先级':'标准')+(open===true?'已建立':open===false?'未建立':'未知'); }).join(' / '):'未知');
        this.ultra.textContent+=' · 原生空调：'+((ultra.climate_control||{}).allowed?'已允许本次 CarPlay 控制':'未授予本次 CarPlay 控制权限');
        self.sync();
    },
    refresh: async function() {
        if (this.disposed) return;
        this.update();if(this.mutating)return;var epoch=this.epoch,read=++this.readEpoch;
        try { var result=await Promise.all([cp.call('tesla_status'),cp.call('status').catch(function() { return {}; })]);if(this.disposed||epoch!==this.epoch||read!==this.readEpoch)return;if(typeof result[0].configured!=='boolean'||!result[0].vehicle)throw Error('Tesla 状态不完整，请核对核心版本。');this.data=result[0];this.host=result[1];this.update(); }
        catch(e) { if(!this.disposed&&epoch===this.epoch&&read===this.readEpoch){this.phase.textContent='无法读取 Tesla 当前状态';this.feedback.textContent=e.message;} }
    },
    action: function(id,label,method,prompt,available,args) {
        var self=this,button=E('button',{id:id,type:'button',class:'btn cbi-button cw-btn',click:async function() {
            if(self.pending||self.disposed||!L.hasViewPermission()||!available())return;
            var before=self.context(),draft=JSON.stringify([self.vin.value,self.enabled.checked,self.driver.value,self.passenger.value]);
            var current=function() { return !self.disposed&&L.hasViewPermission()&&available()&&before===self.context()&&draft===JSON.stringify([self.vin.value,self.enabled.checked,self.driver.value,self.passenger.value]); };
            self.pending=true;self.sync();
            try {
                var values=args?args():[];
                if(prompt&&(!await confirmation.ask({key:'tesla',title:prompt.title,description:typeof prompt.description==='function'?prompt.description():prompt.description,confirm:prompt.confirm,current:current})||!current()))return;
                if(!current())return;
                var epoch=++self.epoch;self.mutating=true;self.feedback.textContent='正在发送请求…';var result=await cp.call.apply(cp,[method].concat(values));
                if(self.disposed||epoch!==self.epoch)return;
                if(typeof result.configured!=='boolean'||!result.vehicle)throw Error('未收到完整操作状态，请刷新核对后再重试。');
                self.data=result;
                if(method==='tesla_configure'){self.vin.value='';self.dirty=false;}
                self.feedback.textContent=method==='tesla_configure'?'配置已保存。本机密钥由主机生成或复用；授权和连接结果见实时状态。':method==='tesla_climate_command'?'已返回单次命令结果，请查看车辆确认状态；页面不会推测车辆温度。':'请求已返回，结果以车辆实时状态为准。';self.update();
            } catch(e) { if(!self.disposed)self.feedback.textContent='操作结果未确认：'+e.message+'。请核对实时状态后重试。'; }
            finally { self.mutating=false;if(!self.disposed){self.pending=false;self.sync();} }
        }},label);this.buttons.push({button:button,available:available});return button;
    },
    render: function(result) {
        var self=this;this.data=result[0];this.host=result[1];this.epoch=0;this.readEpoch=0;this.pending=false;this.mutating=false;this.disposed=false;this.buttons=[];this.dirty=false;this.temperatureDirty=false;
        this.vin=E('input',{id:'tesla-vin',type:'text',autocomplete:'off',autocapitalize:'characters',spellcheck:'false',maxlength:17,placeholder:'输入完整的 17 位 VIN','aria-label':'Tesla VIN'});
        this.enabled=E('input',{id:'tesla-enabled',type:'checkbox','aria-label':'启用 Tesla 连接'});
        this.vin.addEventListener('input',function(){self.dirty=true;});this.enabled.addEventListener('change',function(){self.dirty=true;});
        this.driver=E('input',{id:'tesla-climate-driver',type:'number',step:'any','aria-label':'驾驶位目标温度',placeholder:'先读取车辆温度'});this.passenger=E('input',{id:'tesla-climate-passenger',type:'number',step:'any','aria-label':'副驾目标温度',placeholder:'先读取车辆温度'});
        [this.driver,this.passenger].forEach(function(input){input.addEventListener('input',function(){self.temperatureDirty=true;});});
        this.phase=E('p',{id:'tesla-phase',role:'status'});this.feedback=E('p',{id:'tesla-feedback',class:'cw-foot',role:'status'});this.savedVin=E('span',{id:'tesla-saved-vin'});this.key=E('span',{id:'tesla-key'});this.fingerprint=E('code',{id:'tesla-fingerprint'});this.permission=E('span',{id:'tesla-permission'});this.deadline=note('');this.deadline.id='tesla-pairing-deadline';
        this.driverKey=E('span',{id:'tesla-driver-key'});this.driverFingerprint=E('code',{id:'tesla-driver-fingerprint'});this.climateStatus=E('p',{id:'tesla-climate-status',class:'cw-foot',role:'status'});this.climateValues=E('p',{id:'tesla-climate-values',class:'cw-foot'});this.climateRange=E('p',{id:'tesla-climate-range',class:'cw-foot'});this.climateAge=E('p',{id:'tesla-climate-age',class:'cw-foot'});this.ultra=E('p',{id:'tesla-ultra-state',class:'cw-foot'});
        var configured=function(){return !!self.data.configured&&!!self.data.enabled&&!!self.data.key_ready&&!self.busy();},control=function(){return configured()&&!!(self.data.climate||{}).authorized;};
        var save=this.action('tesla-save','保存 VIN 并准备本机密钥','tesla_configure',{title:'保存 Tesla 配置？',description:'将保存车辆 VIN，并在主机生成或复用本机密钥。已有车辆连接会重新读取配置；私钥保留在主机。',confirm:'保存配置'},function(){return typeof self.data.configured==='boolean'&&!!self.data.vehicle&&!self.busy();},function(){var vin=self.vin.value.trim().toUpperCase();if(vin&&!/^[A-HJ-NPR-Z0-9]{17}$/.test(vin)||!vin&&!self.data.configured)throw Error('请输入 17 位 VIN，不包含 I、O、Q');return [vin||null,self.enabled.checked];});
        var pair=this.action('tesla-pair','开始车内配对','tesla_pair',{title:'开始 Tesla 车内授权？',description:'请将车辆停稳，在车内按车辆提示确认本机公钥。当前申请 Vehicle Monitor 只读车辆数据授权，不代表空调控制权限。配对窗口有时间限制，可以取消。',confirm:'开始配对'},configured);
        var cancel=this.action('tesla-cancel','取消当前任务','tesla_cancel',{title:'取消当前 Tesla 任务？',description:'将关闭本次配对或空调任务并清理连接。已有车辆授权和本机密钥保留；按原连接设置恢复只读数据监测。已发送命令的车辆结果仍需重新读取。',confirm:'取消当前任务'},function(){return self.busy();});
        var connect=this.action('tesla-connect','连接 / 重试','tesla_connect',{title:'连接 Tesla？',description:'将尝试连接已配置车辆并验证本机公钥授权。连接与读取结果见实时状态，失败后可手动重试。',confirm:'连接车辆'},configured);
        var disconnect=this.action('tesla-disconnect','断开车辆','tesla_disconnect',{title:'断开 Tesla 连接？',description:'将停止车辆数据读取、当前任务和自动重连，已有车辆授权与本机密钥保留。之后可手动连接。',confirm:'断开车辆'},function(){return !!self.data.configured;});
        var upgrade=this.action('tesla-climate-pair','升级车辆控制授权','tesla_climate_pair',{title:'申请 Driver 车辆控制授权？',description:'现有 Vehicle Monitor 授权只读。此次需在车内确认 Driver 角色，车辆授予的能力范围广于空调；CowPlay 页面仅发送空调开关与双区目标温度。不会自动升级授权。',confirm:'申请并在车内确认'},configured);
        var query=this.action('tesla-climate-query','读取空调状态','tesla_climate_query',null,function(){return configured()&&!!(self.data.climate||{}).key_ready;});
        var power=function(on){return self.action('tesla-climate-'+(on?'on':'off'),on?'开启空调':'关闭空调','tesla_climate_command',{title:(on?'开启':'关闭')+'车辆空调？',description:'将向当前车辆发送一次空调命令。车辆确认与当前读数分别显示；结果未确认时不会自动重试。',confirm:on?'开启空调':'关闭空调'},control,function(){return ['power',on,undefined,undefined];});};
        var temperature=this.action('tesla-climate-temperature','设置双区目标温度','tesla_climate_command',{title:'设置车辆目标温度？',description:function(){return '将发送一次目标温度命令：驾驶位 '+self.driver.value+' °C，副驾 '+self.passenger.value+' °C。车辆确认不会覆盖当前读数，结果未确认时不会自动重试。';},confirm:'设置目标温度'},function(){var data=(self.data.climate||{}).state;return control()&&self.climateFresh()&&data&&data.min_temp_celsius!=null&&data.max_temp_celsius!=null;},function(){var data=(self.data.climate||{}).state,left=Number(self.driver.value),right=Number(self.passenger.value);if(!self.driver.value||!self.passenger.value||!Number.isFinite(left)||!Number.isFinite(right)||!data||data.min_temp_celsius==null||data.max_temp_celsius==null||left<data.min_temp_celsius||left>data.max_temp_celsius||right<data.min_temp_celsius||right>data.max_temp_celsius)throw Error('请按车辆返回的温度范围填写两个目标温度');return ['temperature',undefined,left,right];});
        var nativeGrant=this.action('tesla-ultra-allow','允许本次 CarPlay 控制空调','tesla_ultra_control',{title:'允许当前 iPhone 控制车辆空调？',description:'允许当前已认证 CarPlay 会话发送空调开关和双区温度命令。不会申请新的车辆权限；换手机、重连、换车或停服后需重新允许，可随时撤销。',confirm:'允许本次会话'},function(){var u=self.data.ultra||{},c=u.climate_control||{};return configured()&&self.climateFresh()&&c.available&&Array.isArray(u.channels)&&u.channels.some(Boolean)&&!c.allowed;},function(){return [true,self.data.ultra.session,self.data.ultra.climate_control.context];});
        var nativeRevoke=this.action('tesla-ultra-revoke','撤销本次 CarPlay 空调控制','tesla_ultra_control',null,function(){return !!((self.data.ultra||{}).climate_control||{}).allowed;},function(){return [false,self.data.ultra.session,self.data.ultra.climate_control.context];});
        this.readings=[];var readings=E('dl',{class:'cw-tesla-readings'},['电量','续航','左前胎压','右前胎压','左后胎压','右后胎压'].map(function(label,i){var dd=E('dd',{id:'tesla-reading-'+i},'—');self.readings.push(dd);return E('div',{},[E('dt',{},label),dd]);}));
        var form=E('form',{id:'tesla-config-form'},[field('车辆 VIN',this.vin),note('已有 VIN 不会回填。首次或更换车辆时输入完整 VIN；留空会保留已保存车辆。'),field('启用 Tesla 连接',this.enabled),save]);form.addEventListener('submit',function(ev){ev.preventDefault();save.click();});
        var on=power(true),off=power(false);
        // One primary action per step; others are neutral or negative.
        [[save,'action'],[pair,'action'],[connect,'action'],[query,'action'],[nativeGrant,'action'],[disconnect,'negative'],[nativeRevoke,'negative']].forEach(function(r){r[0].classList.add('cbi-button-'+r[1]);});
        [cancel,upgrade,on,off,temperature].forEach(function(b){b.classList.add('cbi-button-neutral');});
        this.chips=[0,1,2,3,4].map(function(){return E('span',{class:'cw-tesla-chip',hidden:true});});
        var step=function(n,title,children){return E('div',{class:'cw-section cw-tesla-step'},[E('h3',{},[E('span',{class:'cw-tesla-no','aria-hidden':'true'},String(n)),E('span',{class:'cw-grow'},title),self.chips[n-1]])].concat(children));};
        this.freshness=E('p',{id:'tesla-freshness',class:'cw-foot',role:'status'});
        var nav=E('ul',{class:'cbi-tabmenu cw-tabs'},[E('li',{class:'cbi-tab-disabled'},E('a',{href:L.url('admin/services/cowplay')},'CowPlay 概览')),E('li',{class:'cbi-tab'},E('a',{href:L.url('admin/services/cowplay/tesla'),'aria-current':'page'},'Tesla 车辆'))]);
        this.root=E('div',{class:'cw cw-tesla',id:'tesla-management'},[E('link',{rel:'stylesheet',href:L.resource('cowplay/style.css')}),E('link',{rel:'stylesheet',href:L.resource('cowplay/tesla.css')}),E('div',{class:'cw-head'},[E('div',{class:'cw-grow'},[E('h2',{},'CowPlay · Tesla 车辆')]),E('button',{id:'tesla-refresh',type:'button',class:'btn cbi-button cbi-button-neutral cw-btn',click:function(){self.refresh();}},'刷新状态')]),nav,note('按步骤完成配置、车内配对和连接。离开此页后车辆连接保留。'),this.phase,this.feedback,!L.hasViewPermission()?note('当前 LuCI 会话只有读取权限，车辆管理与空调命令不可用。'):null,
            step(1,'车辆',[E('div',{class:'cw-card'},[row('已保存的车辆',this.savedVin)]),form]),
            step(2,'车内配对',[note('车辆停稳后开始，在车内屏幕确认本机公钥。只申请读取车辆数据的权限；失败不会清除已有授权。'),this.deadline,E('div',{class:'cw-actions'},[pair,cancel])]),
            step(3,'连接与读数',[E('div',{class:'cw-actions'},[connect,disconnect]),this.freshness,readings]),
            step(4,'空调控制（可选）',[this.climateStatus,this.climateValues,this.climateAge,E('div',{class:'cw-actions'},[query,upgrade]),note('升级授权需在车内确认，车辆授予的权限范围大于空调；页面只发送空调开关与双区温度，每条命令只发一次。'),E('div',{class:'cw-actions'},[on,off]),this.climateRange,field('驾驶位目标温度（°C）',this.driver),field('副驾目标温度（°C）',this.passenger),E('div',{class:'cw-actions'},[temperature])]),
            step(5,'CarPlay 原生空调（实验）',[this.ultra,E('div',{class:'cw-actions'},[nativeGrant,nativeRevoke]),note('需先完成第 4 步并读取空调状态，再允许当前 CarPlay 会话控制。换手机、重连或停服后需重新允许；真实车辆效果需现场验收。')]),
            E('details',{class:'cw-tesla-details'},[E('summary',{},'技术详情'),row('只读身份密钥',this.key),row('只读公钥指纹',this.fingerprint),row('只读车辆授权',this.permission),row('独立控制身份',this.driverKey),row('控制公钥指纹',this.driverFingerprint)])].filter(Boolean));
        this.update();var mounted=false;this.pollTesla=function(){if(self.root.isConnected)mounted=true;else if(mounted){self.dispose();return;}else return;return self.refresh();};poll.add(this.pollTesla,2);window.addEventListener('pagehide',function(){self.dispose();},{once:true});return this.root;
    },
    handleSaveApply:null,handleSave:null,handleReset:null
});
