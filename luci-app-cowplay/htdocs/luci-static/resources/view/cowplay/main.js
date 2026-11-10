'use strict';
'require view';
'require form';
'require uci';
'require ui';
'require poll';
'require cowplay.rpc as cp';
'require cowplay.network as network';
'require cowplay.updater as updater';
'require cowplay.confirmation as confirmation';

var diagnosticTabs = [['checks', '实时数据'], ['info', '设备信息'], ['timeline', '时间线'], ['logs', '日志'], ['proc', '进程与配置']];
var tabs = [['overview', '概览'], ['devices', '设备与访问'], ['display', '投屏策略'], ['network', '网络与证书']].concat(diagnosticTabs, [['system', '系统']]);
var subtitles = {
    overview: '连接状态、视频传输与会话记录。',
    devices: '管理已保存的 iPhone、屏幕授权和访问令牌。',
    display: '设置新屏幕默认值和画面请求上限。',
    network: '',
    diag: '查看实测数据、设备信息、事件与日志。',
    system: '管理 CowPlay 服务、日志与配置备份。'
};
function val(v, unit, digits) { return v == null || !Number.isFinite(+v) ? '—' : (+v).toFixed(digits || 0) + (unit || ''); }
function card(children) { return E('div', {class: 'cw-card'}, children); }
function note(s) { return E('p', {class: 'cw-foot'}, s); }
function section(t, body, aside) { return E('section', {class: 'cbi-section cw-section'}, [E('h3', {}, [t, aside ? E('small', {class: 'cw-aside'}, aside) : null].filter(Boolean)), E('div', {class: 'cbi-section-node'}, body)]); }
function row(k, v) { return E('div', {class: 'cw-row'}, [E('span', {class: 'cw-key'}, k), typeof v === 'string' ? E('span', {class: 'cw-value'}, v) : v]); }
function status(text, level) { return E('span', {class: 'cw-state', 'data-state': level}, text); }
function table(headers, rows) {
    return E('div', {class: 'cw-table-wrap'}, E('table', {class: 'table cw-table'}, [E('thead', {}, E('tr', {class: 'tr table-titles'}, headers.map(function(t) { return E('th', {class: 'th'}, t); }))), E('tbody', {}, rows.length ? rows.map(function(r) { return E('tr', {class: 'tr'}, r.map(function(v) { return E('td', {class: 'td'}, v); })); }) : E('tr', {class: 'tr'}, E('td', {class: 'td', colspan: headers.length}, '暂无记录')))]));
}
function observed(v, yes, no) { return v === true ? yes : v === false ? no : '未知'; }
function linkState(link, key) {
    var names = {idle: '等待连接', active: '连接中', ok: '已完成', fail: '失败'};
    return link[key + '_detail'] || names[link[key]] || '未知';
}
function panel(title, name, state, rows) {
    return E('div', {class: 'cw-panel'}, [E('div', {class: 'cw-panel-head'}, [E('h4', {}, title), state]), E('p', {class: 'cw-device-name'}, name), E('div', {}, rows)]);
}
var metricSources = {
    '车机 ⟷ F50': '车机浏览器的往返探测上报',
    'F50 ⟷ iPhone': 'F50 无线驱动采样',
    '画面': '浏览器绘制与解码上报；停住次数统计最近 10 分钟',
    'F50': '设备 CPU 与温度采样'
};
function measurements(checks, stats) {
    stats = stats || {};
    var wifiReason = {driver_unavailable: '当前无线驱动未提供此手机的信号、速率和重传指标，无法测量。', query_failed: '无线驱动查询失败，无法取得当前手机的采样。', disconnected: '当前没有已连接的 iPhone。', stale: '当前手机的无线采样已过期或连接已变化。', unidentified: '尚未识别当前 iPhone 对应的无线客户端。', station_missing: '当前 iPhone 不在所选无线接口的客户端列表中。', no_interface: '尚未找到用于 CarPlay 的无线接口。'}[(stats.phone || {}).wifi_status] || '没有可关联到当前 iPhone 的新鲜无线采样。';
    var car = stats.car || {}, background = car.visible === false;
    var rawCar = {'延迟': car.rtt_ms, '抖动': car.jitter_ms, '丢包': car.loss_pct};
    var groups = [];
    checks.forEach(function(c) { if (groups.indexOf(c.group) < 0) groups.push(c.group); });
    return E('div', {class: 'cw-measurements'}, groups.map(function(g) {
        return E('section', {class: 'cw-panel'}, [E('h4', {}, g), note(metricSources[g] || '服务采样'), note(g === '车机 ⟷ F50' && background ? '投屏页在后台；显示最新原始上报，读数可能受浏览器后台节流影响。' : g === '画面' && !((stats.server || {}).fps_in > 0) ? '当前没有新视频帧；静止画面时即时帧率与解码耗时可能没有新值。' : checks.filter(function(c) { return c.group === g && c.known; }).length ? '' : g === '车机 ⟷ F50' ? '没有当前屏幕的有效上报：可能未连接、处于后台或上报已过期。' : g === 'F50 ⟷ iPhone' ? wifiReason : g === '画面' ? '等待动态画面和浏览器上报；静止画面不能据此判断流畅度。' : '尚未取得设备采样。'), E('dl', {class: 'cw-samples'}, checks.filter(function(c) { return c.group === g; }).map(function(c) {
            var raw = background && c.group === '车机 ⟷ F50' ? rawCar[c.name] : null;
            var reading = c.known && c.value != null && Number.isFinite(c.value) ? c.text : Number.isFinite(raw) ? val(raw, c.name === '丢包' ? ' %' : ' ms', c.name === '丢包' ? 1 : 0) : '—';
            return E('div', {class: 'cw-sample'}, [E('dt', {}, c.name === '延迟' ? '往返时间 RTT' : c.name === '停住' ? '停住次数（10 分钟）' : c.name), E('dd', {class: 'cw-reading'}, reading)]);
        }))]);
    }));
}
return view.extend({
    load: function() {
        var route = location.hash.slice(1); if (route === 'diag') route = 'checks';
        this.maps = []; this.pendingActions = new Map(); this.actionEpoch = 0; this.formEpoch = 0; this.page = tabs.some(function(t) { return t[0] === route; }) ? route : 'overview'; this.diagTab = diagnosticTabs.some(function(t) { return t[0] === route; }) ? route : 'checks'; if (diagnosticTabs.some(function(t) { return t[0] === route; })) this.page = 'diag'; this.sampling = false; this.lastSample = null; this.diagEpoch = 0; this.logLevel = ''; this.logModule = ''; this.timelineScope = 'current'; this.disableDdnsPending = false; this.editState = 'clean';
        return Promise.all([uci.load('cowplay'), uci.load('wireless'), uci.load('network'), cp.call('status').catch(function(e) { return {error: e.message}; }), cp.call('phones').catch(function(e) { return {error: e.message}; }), cp.call('screens').catch(function(e) { return {error: e.message}; }), cp.call('cert').catch(function(e) { return {error: e.message}; }), cp.call('proc').catch(function(e) { return {error: e.message}; }), cp.call('service_info').catch(function(e) { return {error: e.message}; })]);
    },
    actionUI: function() { return JSON.stringify([this.actionEpoch, location.hash, this.page, this.diagTab]); },
    actionContext: function() { return JSON.stringify([this.actionUI(), this.data.pid, this.data.build, this.data.context]); },
    actionPrompt: function(label, method, args, dangerous) {
        var description;
        if (method === 'cmd') {
            var cmd = args[0], phone = (this.phones.phones || []).find(function(p) { return p.id === args[1]; });
            if (cmd === 'switchPhone' || cmd === 'connect') description = '如有正在进行的 CarPlay 或连接尝试，会先停止，随后连接' + (phone ? phone.name : '所选 iPhone') + '。画面和声音会中断，已有配对保留。';
            if (cmd === 'openPairWindow') description = '如有正在进行的 CarPlay、连接或配对尝试，会先停止，再打开新的配对窗口。已有手机配对保留。';
            if (cmd === 'forgetPhone') description = '将删除' + (phone ? phone.name : '这台 iPhone') + '的配对；之后需要重新配对。其他手机不受影响。';
        } else if (method === 'service') description = args[0] === 'enable' || args[0] === 'disable' ? '将' + (args[0] === 'enable' ? '开启' : '关闭') + ' CowPlay 开机自动启动；当前服务状态不变。' : args[0] === 'start' ? '将启动 CowPlay，随后可以连接已保存的 iPhone。' : '将' + (args[0] === 'stop' ? '停止' : '重启') + ' CowPlay，当前画面和声音会中断。配对与配置保留。';
        else description = ({client_remove: '将立即撤销所选屏幕的访问授权，停止其画面和触控；再次使用需要重新授权。', token_reset: '将生成新的访问令牌，旧令牌不能再用于新增授权。已授权设备保留。', config_import: '将用所选文件替换 CowPlay 配置并刷新页面，未保存的编辑会丢弃。配对和配件身份保留；部分配置需要重启后生效。', mfi_upload: '将验证并保存所选配件证书及匹配私钥，原文件保留。需要手动重启 CowPlay 后生效，配对保留。', cert_upload: '将立即验证并替换 HTTPS 证书，并停止自动续期。访问可能短暂中断，已有配对保留。', cert_rollback: '将立即换回上一张 HTTPS 证书。访问可能短暂中断，已有配对保留。', cert_renew: '将使用已保存的域名和 DNS 凭据申请或续期 HTTPS 证书。签发结果见任务状态；通过校验的生产证书才会替换当前证书。', ddns_token: '将保存当前输入的证书 DNS API 凭据，供后续证书申请和续期使用。'})[method];
        return description || dangerous ? {title: label + '？', description: description || '确认执行此操作？', confirm: '确认' + label} : null;
    },
    syncActions: function() {
        this.root.querySelectorAll('button[data-cw-action]').forEach(function(b) { b.syncPending(); });
    },
    action: function(label, method, args, dangerous, done, available, failed, settled) {
        var self = this, key = method + ':' + label + ':' + (typeof args === 'function' ? '' : JSON.stringify(args || []));
        var b = E('button', {type: 'button', 'data-cw-action': key, class: 'btn cbi-button ' + (dangerous ? 'cbi-button-negative' : 'cbi-button-neutral') + ' cw-btn', click: async function() {
            if (self.updateBusy || self.pendingActions.has(key) || !L.hasViewPermission() || available && !available()) return;
            var token = {phase: 'confirm'}, source = self.actionUI(), context = self.actionContext(), result, started = false;
            var current = function() { return !self.updateBusy && !token.cancelled && self.pendingActions.get(key) === token && L.hasViewPermission() && context === self.actionContext(); };
            self.pendingActions.set(key, token); self.syncActions();
            try {
                var prompt = self.actionPrompt(label, method, typeof args === 'function' ? [] : args || [], dangerous);
                if (prompt && (!await confirmation.ask(Object.assign({key: key, current: current}, prompt)) || !current())) return;
                if (!current()) return;
                token.phase = 'running'; self.syncActions();
                var values = typeof args === 'function' ? args() : args || []; started = true;
                result = await cp.call.apply(cp, [method].concat(values));
                if (token.cancelled || source !== self.actionUI() || self.pendingActions.get(key) !== token) return;
                if (method === 'selftest' && (result.context_current === false || result.context && JSON.stringify(result.context) !== JSON.stringify(self.data.context))) return;
                if (method === 'config_import' || method === 'factory_reset') { location.reload(); return; }
                if (done) done(result);
                await self.refresh();
                if (method.indexOf('cert_') === 0) { self.certData = await cp.call('cert'); self.updateCert(); }
                if (method === 'service') { self.serviceData = await cp.call('service_info'); self.update(); }
            } catch (e) { if (source === self.actionUI()) { if (failed) failed(e); cp.failure(e); } }
            finally { if (started && settled) settled(result); if (self.pendingActions.get(key) === token) self.pendingActions.delete(key); self.syncActions(); }
        }}, label);
        b.syncPending = function() { var pending = self.pendingActions.get(key); b.disabled = !!self.updateBusy || !!pending || !L.hasViewPermission() || !!(available && !available()); b.textContent = pending ? pending.phase === 'confirm' ? '等待确认…' : '处理中…' : label; };
        b.disarm = function() { var pending = self.pendingActions.get(key); if (pending) pending.cancelled = true; confirmation.cancel(key); };
        b.syncPending();
        return b;
    },
    map: function(name) { var m = new form.Map(name || 'cowplay'); this.maps.push(m); return m; },
    formSection: function(m, id, type, title) {
        var s = m.section(form.TypedSection, type, title); s.anonymous = true; s.addremove = false; s.cfgsections = function() { return [id]; }; return s;
    },
    option: function(s, kind, key, title, opts, description) {
        var o = s.option(kind === 'flag' ? form.Flag : kind === 'list' ? form.ListValue : form.Value, key, title, description);
        o.rmempty = false;
        if (kind === 'list') (opts || []).forEach(function(v) { o.value(v[0], v[1]); });
        if (kind === 'flag') o.default = '0';
        return o;
    },
    unavailable: function(s, key, title, reason) {
        var o = s.option(form.DummyValue, key, title);
        o.cfgvalue = function() { return reason; };
        return o;
    },
    content: function(s, key, title, node) {
        var o = s.option(form.DummyValue, key, title);
        o.renderWidget = function() { return node; };
        return o;
    },
    displayForm: function() {
        var m = this.map(), d = this.formSection(m, 'screen', 'defaults', '新屏幕默认值'), s, caps=(this.data.display_limits||{}).receiver;
        this.option(d, 'list', 'quality', '画质', [['auto', '自动'], ['smooth', '流畅 · 30 帧'], ['sharp', '清晰']]);
        this.option(d, 'list', 'call', '通话与 Siri', [['bt', '车机蓝牙'], ['web', '屏幕麦克风']]);
        this.option(d, 'list', 'rail_side', '控制栏位置', [['left', '左侧'], ['right', '右侧']]);
        s = this.formSection(m, 'main', 'cowplay', '向 iPhone 请求的参数');
        var aliases = caps && caps.resolution_aliases || {}, choices = ['720p', '1080p', '1440p', '2160p'].filter(function(k) { return aliases[k]; }).map(function(k) { return [k, (k === '2160p' ? '4K' : k) + ' · ' + aliases[k].replace('x', '×')]; });
        var saved = uci.get('cowplay', 'main', 'max_res') || '1080p';
        var canonical = Object.keys(aliases).find(function(k) { return aliases[k] === saved; }) || saved;
        if (!choices.some(function(v) { return v[0] === canonical; })) choices.push([canonical, canonical + '（原配置）']);
        var size = this.option(s, 'list', 'max_res', '请求分辨率上限', choices, '这是接收端请求上限；实际画面由 iPhone 接受后返回。');
        size.readonly = !caps;
        size.cfgvalue = function(id) { var value = uci.get("cowplay", id, "max_res") || "1080p"; return Object.keys(aliases).find(function(k) { return aliases[k] === value; }) || value; };
        var fps = this.option(s, 'list', 'max_fps', '请求帧率上限', [['30', '30 帧'], ['60', '60 帧']], '静止画面可能低于此值。');
        var savedFps = uci.get('cowplay', 'main', 'max_fps') || '60';
        if (savedFps !== '30' && savedFps !== '60') {
            fps.value('', '原保存值 ' + savedFps + ' 帧（保留；请选择 30 / 60 修改）');
            fps.rmempty = true; fps.remove = function() {}; // An untouched legacy setting must survive unrelated saves.
        }
        fps.cfgvalue = function(id) { var v = uci.get('cowplay', id, 'max_fps') || '60'; return v === '30' || v === '60' ? v : ''; };
        fps.readonly = !caps;
        return m.render();
    },
    devicesForm: function() {
        var m = this.map(), s = this.formSection(m, 'main', 'cowplay', '自动连接');
        var preferred = this.option(s, 'list', 'preferred', '首选 iPhone', [['', '最近使用的 iPhone']]);
        preferred.rmempty = true;
        (this.phones.phones || []).forEach(function(p) { preferred.value(p.id, p.name); });
        this.option(s, 'flag', 'ask_when_multi', '多台在线时在车机上询问');
        s = this.formSection(m, 'main', 'cowplay', '访问令牌');
        this.content(s, '_token', '当前令牌', this.tokenDisplay);
        this.content(s, '_token_actions', '令牌操作', this.tokenActions);
        this.unavailable(s, '_token_note', '说明', '重置不自动撤销已授权设备；移除屏幕后立即停止其画面与触控。');
        return m.render();
    },
    networkForm: function() {
        var self = this, m = this.map(), s = this.formSection(m, 'main', 'cowplay', '访问地址与端口');
        var domain = this.option(s, 'value', 'domain', '访问域名', null, '须与证书域名一致。');
        domain.datatype = 'hostname'; domain.rmempty = true;
        domain.cfgvalue = function(id) { var configured = uci.get('cowplay', id, 'domain'); if (configured) return configured; try { var host = new URL((self.data.network || {}).origin).hostname; return /^[0-9.]+$/.test(host) || host.indexOf(':') >= 0 ? '' : host; } catch (_) { return ''; } };
        this.portOption = this.option(s, 'value', 'port', 'HTTPS 端口', null, '默认 18443；支持 443 和确认覆盖占用。');
        this.portOption.datatype = 'and(uinteger,range(1,65535))';
        this.portOption.onchange = function(ev, id, value) { network.inspect(self, Number(value)); };
        this.content(s, '_port_status', '占用情况', this.portStatus);
        this.content(s, '_address', '当前地址', this.runAddress);
        this.content(s, '_lan_ip', '监听 / 局域网地址', this.lanAddress);
        s = this.formSection(m, 'main', 'cowplay', '自动续期');
        this.option(s, 'list', 'ddns_provider', 'DNS 服务商', [['cloudflare', 'Cloudflare'], ['duckdns', 'DuckDNS']]);
        this.content(s, '_dns_token', '证书 DNS API 令牌', this.dnsToken);
        this.content(s, '_dns_token_save', '凭据操作', this.dnsTokenSave);
        return m.render();
    },
    wirelessInfo: function() {
        var net = this.data.network || {}, iface = uci.get('cowplay', 'main', 'wifi_iface');
        var aps = uci.sections('wireless', 'wifi-iface').filter(function(w) { return w.mode === 'ap' && (iface ? w['.name'] === iface || w.ifname === iface : w.ssid === net.ssid); });
        var ap = aps.length === 1 ? aps[0] : null, channel = ap && uci.get('wireless', ap.device, 'channel');
        var network = ap && ap.network, networks = Array.isArray(network) ? network : String(network || '').split(' ');
        var addresses = networks.filter(Boolean).map(function(n) { var ip = uci.get('network', n, 'ipaddr'), mask = uci.get('network', n, 'netmask'); return n + ' · ' + (Array.isArray(ip) ? ip.join(', ') : ip || '地址未知') + (mask ? ' / ' + mask : ''); });
        return section('无线与局域网', card([
            row('SSID', ap ? ap.ssid || '—' : net.ssid || '无法唯一定位热点'),
            row('信道 / 频宽', ap ? 'CH ' + (channel || '未知') + ' · ' + (uci.get('wireless', ap.device, 'htmode') || '未知') : '—'),
            row('接口 / 网段', addresses.join('；') || '请在网络 › 接口中核对'),
            E('div', {class: 'cw-actions'}, [E('a', {href: L.url('admin/network/wireless'), class: 'btn cbi-button cbi-button-neutral'}, 'Wi-Fi 设置 ↗'), E('a', {href: L.url('admin/network/network'), class: 'btn cbi-button cbi-button-neutral'}, '局域网设置 ↗')]),
            note('修改 Wi-Fi 会中断连接，之后需重启 CowPlay。'),
            E('details', {'data-network-help': ''}, [E('summary', {}, '车机网络说明'),
                note('中国大陆 Model 3 / Y 手册列出 5 GHz 149–165；165 需核对带宽。以本车手册为准。'),
                note('域名须解析到 F50 可达地址；注意与蜂窝 WAN、VPN 网段冲突。车机访问限制以实车为准。'),
                E('div', {class: 'cw-actions'}, [E('a', {href: 'https://www.tesla.cn/ownersmanual/model3/zh_cn/GUID-1FE9620C-3D7F-4FD3-BBD9-28DD342AC150.html', target: '_blank', rel: 'noopener'}, 'Tesla Wi-Fi 手册 ↗'), E('a', {href: 'https://www.iana.org/assignments/iana-ipv4-special-registry/', target: '_blank', rel: 'noopener'}, '网段说明 ↗')])])
        ]));
    },
    ddnsInfo: function() {
        var self = this, configured = uci.get('cowplay', 'main', 'ddns_enabled') === '1' && uci.get('cowplay', 'main', 'ddns_mode') !== 'luci', active = ((this.data.network || {}).ddns || {}).enabled === true;
        return section('域名解析', card([
            E('div', {class: 'cw-actions'}, [E('a', {href: L.url('admin/services/ddns')}, '动态 DNS 设置 ↗'), E('a', {href: L.url('admin/network/dhcp')}, '本地 DNS 设置 ↗')]),
            active && configured && !this.disableDdnsPending ? E('button', {type: 'button', class: 'btn cbi-button cbi-button-neutral', disabled: !L.hasViewPermission() || null, click: function() { self.disableDdnsPending = true; self.setEditState('dirty'); self.ddnsPanel.replaceChildren(self.ddnsInfo()); }}, '停用旧内置 DDNS') : null,
            active ? note(this.disableDdnsPending ? '停用将在保存并应用后生效。' : '旧内置 DDNS 仍启用，配置系统 DDNS 后可停用。') : this.data.error ? note('DDNS 状态未知。') : null
        ].filter(Boolean)));
    },
    systemForm: function() {
        var m = this.map(), s = this.formSection(m, 'main', 'cowplay', '日志');
        this.option(s, 'list', 'log_level', '记录级别', [['info', '信息'], ['debug', '调试']], '调试日志仅在排查时使用。日志在“日志”页按级别和来源查看；存储沿用已有配置。');
        return m.render();
    },
    select: function(route) {
        if (this.importRunning) return;
        this.actionEpoch++; confirmation.cancel();
        if (this.hideManagementToken) this.hideManagementToken();
        var diagnostic = diagnosticTabs.some(function(t) { return t[0] === route; }), page = diagnostic ? 'diag' : route;
        if (diagnostic) this.diagTab = route;
        this.page = page; history.replaceState(null, '', '#' + route);
        this.root.querySelectorAll('[data-page]').forEach(function(p) { p.hidden = p.dataset.page !== page; });
        this.root.querySelectorAll('[role=tab][data-target]').forEach(function(b) { var on = b.dataset.target === route; b.setAttribute('aria-selected', String(on)); b.parentElement.className = on ? 'cbi-tab' : 'cbi-tab-disabled'; });
        this.intro.textContent = diagnostic ? '' : subtitles[page];
        this.updateFooter();
        if (page === 'diag') this.loadDiag();
    },
    refresh: function() {
        var self = this;
        return Promise.all([cp.call('status'), cp.call('phones'), cp.call('screens'), cp.call('service_info').catch(function(e) { return {error: e.message}; }), this.page === 'network' ? cp.call('cert').catch(function(e) { return {error: e.message}; }) : Promise.resolve(null)]).then(function(r) {
            self.data = r[0]; self.phones = r[1]; self.screens = r[2]; self.serviceData = r[3]; if (r[4]) self.certData = r[4]; self.update();
        }).catch(function(e) {
            self.data = {error: e.message}; self.phones = {}; self.screens = {};
            self.update();
        });
    },
    update: function() {
        if(this.displaySupport){var c=this.data.display_limits||{},r=c.requested,actual=(this.data.stats||{}).car||{};this.displaySupport.replaceChildren(row('iPhone 支持范围',c.iphone?c.iphone.note:'未知，尚未取得能力信息'),row('请求参数',r?r.width+'×'+r.height+' · 最高 '+r.fps+' 帧 · '+(r.hevc?'H.265':'H.264'):'—'),row('实际解码尺寸',actual.width&&actual.height?actual.width+'×'+actual.height:(this.data.hub||{}).video?'已收到媒体；等待当前浏览器解码上报':'尚未收到画面'));}
        var self = this, d = this.data, st = d.stats || {}, f = st.f50 || {}, car = st.car || {}, server = st.server || {}, net = d.network || {}, hub = d.hub || {}, link = d.link || {};
        this.error.textContent = d.error ? 'CowPlay 服务尚不可用：' + d.error : '';
        this.pill.textContent = d.error ? '服务状态未知' : '运行中 · ' + Math.floor((d.uptime_s || 0) / 60) + ' 分钟';
        this.version.textContent = '无线 CarPlay 接收端 · ' + (d.version || '—');
        var displayNames = {BROWSER_FRAME_RENDERED: '浏览器已绘制', WAITING_BROWSER_REPORT: '等待浏览器上报'};
        this.overview.replaceChildren(
            section('连接状态', E('div', {class: 'cw-endpoints'}, [
                panel('车机屏幕', d.viewer ? d.viewer.name : d.error ? '未知' : '尚未连接', status(observed(hub.viewer, '网页已连接', '网页未连接'), hub.viewer ? 'ok' : 'idle'), [
                    row('画面上报', displayNames[hub.display] || hub.display || '未知'),
                    row('解码尺寸', car.width && car.height ? car.width + '×' + car.height : '—'),
                    row('解码器', car.decoder || '—')]),
                panel('F50 接收端', 'CowPlay ' + (d.version || '—'), status(d.error ? '服务状态未知' : '服务接口可用', d.error ? 'idle' : 'ok'), [
                    row('CPU', val(f.cpu, '%')), row('温度', val(f.temp, ' °C')),
                    row('可用内存', val(f.mem_free_mb, ' MB') + ' / ' + val(f.mem_total_mb, ' MB'))]),
                panel('iPhone', d.phone ? d.phone.name : d.error ? '未知' : '尚未连接', status(observed(hub.phone, '会话已建立', '会话未建立'), hub.phone ? 'ok' : 'idle'), [
                    row('蓝牙 / iAP2', linkState(link, 'bluetooth')), row('Wi-Fi', linkState(link, 'wifi')),
                    row('CarPlay', linkState(link, 'carplay')), row('媒体输入', observed(hub.video, '已收到视频', '未收到视频'))])
            ]), '每 2 秒刷新'),
            section('视频与传输', card([E('div', {class: 'cw-kpis'}, [
                ['输入帧率', val(server.fps_in, ' fps', 1), 'iPhone → F50'],
                ['绘制帧率', val(car.fps, ' fps', 1), '车机浏览器上报'],
                ['往返时间 RTT', val(car.rtt_ms, ' ms'), '车机 ↔ F50'],
                ['视频码率', val(server.bitrate_bps == null ? null : server.bitrate_bps / 1e6, ' Mbps', 1), 'F50 接收统计']
            ].map(function(x) { return E('div', {class: 'cw-kpi'}, [E('span', {}, x[0]), E('strong', {class: 'cw-reading'}, x[1]), E('small', {}, x[2])]); })),
                note('向 iPhone 请求：' + (d.spec ? d.spec.width + '×' + d.spec.height + ' · 最高 ' + d.spec.fps + ' fps · ' + (d.spec.hevc ? 'H.265' : 'H.264') : '—') + (d.automatic_fps_limit === 30 ? ' · 自动降帧已生效' : ''))])),
            section('会话记录', card(table(['车机屏幕', 'iPhone', '请求画面', '开始', '状态'], (d.sessions || []).map(function(s) { var current = !s.ended_ms && st.session && s.id === st.session.id; return [current && d.viewer ? d.viewer.name + (d.viewer.ip ? ' · ' + d.viewer.ip : '') : '—', s.phone || '—', current && d.spec ? d.spec.width + '×' + d.spec.height + ' · ' + d.spec.fps + ' fps' : '—', new Date(s.started_ms).toLocaleTimeString(), s.ended_ms ? '已断开' : current && hub.display === 'BROWSER_FRAME_RENDERED' ? status('浏览器已绘制', 'ok') : s.first_frame_ms ? '已收到媒体' : '连接中']; }))))
        );
        this.phoneTable.replaceChildren(card([table(['名称', '状态', '蓝牙地址', '最近使用', '自动连接', '操作'], (this.phones.phones || []).map(function(p) {
            return [p.name, p.current ? status('当前 iPhone', 'ok') : p.online ? '在线' : '离线', p.bt || '—', p.last_ms ? new Date(p.last_ms).toLocaleString() : '—', p.preferred ? '首选' : '按最近使用', E('div', {class: 'cw-actions'}, [p.current ? E('span', {}, '当前 iPhone') : p.online ? self.action('切换', 'cmd', ['switchPhone', p.id]) : null, !p.current ? self.action('忘记', 'cmd', ['forgetPhone', p.id], true) : null].filter(Boolean))];
        })), E('div', {class: 'cw-actions cw-end'}, [this.action('＋ 添加 iPhone', 'cmd', ['openPairWindow'], !!d.phone)])]), this.phones.pairing && this.phones.pairing.remaining_s != null ? note('配对窗口：' + this.phones.pairing.remaining_s + ' 秒；请在新 iPhone 上打开 设置 › 蓝牙 并选择“' + (this.phones.pairing.adapter_name || 'cowplay') + '”。') : note('添加新 iPhone 将结束当前 CarPlay，已有配对会保留。'));
        var pairing = this.phones.pairing;
        if (pairing && pairing.request != null && /^\d{6}$/.test(pairing.passkey || '')) this.phoneTable.append(section('蓝牙配对确认', card([row('配对码', E('code', {}, pairing.passkey.slice(0, 3) + ' ' + pairing.passkey.slice(3))), note('需要在 CowPlay 和 iPhone 两端确认。请打开配对页核对数字，再确认或拒绝；首次打开需输入上方的访问令牌。'), net.origin ? E('a', {href: net.origin.replace(/\/$/, '') + '/pair', target: '_blank', rel: 'noopener', class: 'btn cbi-button cbi-button-positive'}, '打开配对页确认或拒绝') : note('访问地址暂不可用，请在车机或手机配对页完成核码。')])));
        this.screenTable.replaceChildren(card(table(['屏幕', '浏览器 / 地址', '画面区', '最近使用', '操作'], (this.screens.screens || []).map(function(s) {
            return [s.name || '车机屏幕', s.ua + ' · ' + s.ip, s.window ? s.window.rw + '×' + s.window.rh : '—', new Date(s.last_ms).toLocaleString(), self.action('移除授权', 'client_remove', [s.id], true)];
        }))));
        this.lanAddress.textContent = net.lan_ip || '—';
        if (this.ddnsPanel) this.ddnsPanel.replaceChildren(this.ddnsInfo());
        this.runAddress.replaceChildren(net.origin ? E('a', {href: net.origin, target: '_blank', rel: 'noopener'}, net.origin) : E('span', {}, '未知；服务恢复后刷新'));
        var enabled = this.serviceData && this.serviceData.enabled;
        this.runtime.replaceChildren(card([row('状态', d.error ? '状态未知' : '运行中 · PID ' + d.pid + ' · ' + Math.floor((d.uptime_s || 0) / 60) + ' 分钟'), row('开机自动启动', typeof enabled === 'boolean' ? E('div', {class: 'cw-actions'}, [status(enabled ? '已开启' : '已关闭', enabled ? 'ok' : 'idle'), this.action(enabled ? '关闭开机自启' : '开启开机自启', 'service', [enabled ? 'disable' : 'enable'])]) : E('a', {href: L.url('admin/system/startup')}, '状态未知；打开系统 › 启动项 ↗')), row('进程守护', (this.procData.process || {}).launcher || '未测得'), row('操作', E('div', {class: 'cw-actions'}, [this.action('启动服务', 'service', ['start']), this.action('重启服务', 'service', ['restart'], true), this.action('停止服务', 'service', ['stop'], true)]))]));
        var sys = this.procData.system || {};
        this.about.replaceChildren(card([row('当前版本', d.version || '—'), row('构建', d.build || '—'), row('架构', sys.arch || '—'), row('系统', [sys.openwrt, sys.kernel].filter(Boolean).join(' · ') || '—'), row('蓝牙运行时', sys.bluez || '—'), row('许可证', 'GPL-3.0-only')]));
        if (this.lastSample && JSON.stringify(this.lastSample.context) !== JSON.stringify(d.context)) this.lastSample = null;
        if (this.mfiStatus) this.mfiStatus.textContent = (d.identity || {}).pending_restart ? '新身份已保存，等待手动重启 CowPlay；当前会话继续使用原身份。' : d.error ? '无法读取当前身份状态' : '当前服务已加载配件身份；文件内容不回显。';
        if (this.segmentResult && JSON.stringify(this.segmentResult.context) !== JSON.stringify(d.context)) this.segmentResult = null;
        if (d.error) { ++this.diagEpoch; this.diagBody.replaceChildren(note('当前无法读取服务诊断。服务恢复后会自动刷新；已保存的手机与屏幕记录不会因此删除。')); }
        else if (this.page === 'diag') this.loadDiag();
        this.updateCert(); this.updateFooter(); this.syncActions();
    },
    replaceDiag: function(content) {
        var positions = Array.from(this.diagBody.querySelectorAll('[data-scroll]')).map(function(e) { return [e.dataset.scroll, e.scrollTop, e.scrollLeft]; });
        var x = window.scrollX, y = window.scrollY;
        if (content.length === this.diagBody.children.length && content.every(function(e, i) { return e.isEqualNode(this.diagBody.children[i]); }, this)) return;
        this.diagBody.replaceChildren.apply(this.diagBody, content);
        positions.forEach(function(p) { var e = this.diagBody.querySelector('[data-scroll="' + p[0] + '"]'); if (e) { e.scrollTop = p[1]; e.scrollLeft = p[2]; } }, this);
        window.scrollTo(x, y);
    },
    rawOutput: function(id, text) {
        var pre = E('pre', {class: 'cw-pre cw-raw', 'data-scroll': id, tabindex: '0'}, text);
        return E('div', {}, [E('div', {class: 'cw-actions'}, E('button', {type: 'button', class: 'btn cbi-button cbi-button-neutral', click: function() { pre.scrollTop = pre.scrollHeight; }}, '滚动到底部')), pre]);
    },
    loadDiag: function() {
        var self = this, tab = this.diagTab, key = JSON.stringify([tab, this.sampling, this.logLevel, this.logModule, this.timelineScope, this.data.context]);
        if (this.diagPending && this.diagPending.key === key) return this.diagPending.promise;
        var epoch = ++this.diagEpoch;
        if (this.data.error || this.page !== 'diag') return Promise.resolve();
        this.root.querySelectorAll('[data-diag]').forEach(function(b) { var on = b.dataset.diag === tab; b.setAttribute('aria-selected', String(on)); b.parentElement.className = on ? 'cbi-tab' : 'cbi-tab-disabled'; });
        var request = tab === 'checks' && !this.sampling ? Promise.resolve(this.lastSample || {diag: {checks: []}}) : tab === 'logs' ? cp.call('logs', this.logLevel, 150, this.logModule) : tab === 'timeline' ? cp.call('timeline', this.timelineScope) : cp.call(tab === 'info' || tab === 'proc' ? 'proc' : 'diag');
        var pending = request.then(function(r) {
            if (epoch !== self.diagEpoch || self.data.error || self.page !== 'diag' || tab !== self.diagTab) return;
            var content = [], st = self.data.stats || {}, car = st.car || {}, f = st.f50 || {}, phone = st.phone || {}, viewer = self.data.viewer || {}, win = viewer.window || {}, spec = self.data.spec;
            if (tab === 'checks') {
                var d = r.diag; if (self.sampling) self.lastSample = r;
                if (!d) { self.diagBody.replaceChildren(note('暂时无法读取诊断。')); return; }
                var stats = r.stats || {}, actual = stats.car || {}, server = stats.server || {}, device = stats.f50 || {}, requested = self.data.spec;
                var samplingButton = E('button', {type: 'button', class: 'btn cbi-button cbi-button-neutral', click: function() { self.sampling = !self.sampling; this.textContent = self.sampling ? '停止采样' : '开始采样'; self.loadDiag(); }}, self.sampling ? '停止采样' : '开始采样');
                content = [section('实时数据', [E('div', {class: 'cw-actions'}, samplingButton),
                    note(stats.ts_ms ? (self.sampling ? '采样中 · ' : '采样已停止 · ') + '最后读取 ' + new Date(stats.ts_ms).toLocaleString() : '点击“开始采样”读取设备指标；投屏页保持前台可取得绘制与网络上报。'),
                    stats.ts_ms ? measurements(d.checks || [], stats) : note('尚未开始采样'),
                    stats.ts_ms ? card([
                        row('实际解码尺寸', actual.width && actual.height ? actual.width + '×' + actual.height : '当前查看通道尚无有效画面'),
                        row('请求尺寸', requested ? requested.width + '×' + requested.height : '尚未声明'),
                        row('累计收到视频帧', val(server.frames_in) + ' · ' + val(server.bytes_in, ' 字节') + '（服务启动以来）'),
                        row('当前输入', val(server.fps_in, ' fps', 1) + ' · ' + val(server.bitrate_bps == null ? null : server.bitrate_bps / 1000000, ' Mbps', 2)),
                        row('可用内存', val(device.mem_free_mb, ' MB')),
                        row('设备 / 屏幕采样年龄', val(device.sample_age_ms, ' ms') + ' / ' + val(stats.car_sample_age_ms, ' ms'))
                    ]) : null].filter(Boolean)),
                    section('连接检查与导出', card([E('div', {class: 'cw-actions'}, [self.action('开始检查', 'selftest', [], false, function(r) { self.segmentResult = r; self.loadDiag(); }), self.action('下载诊断包', 'bundle', [], false, function(r) { cp.download('cowplay-diag.json', r); })]),
                        note('优先读取当前协议探测，必要时发送 ping 并请求视频帧。静止画面可无新帧；音频接收与现场声音输出分别确认。'),
                        self.segmentResult ? table(['观测项', '状态', '返回数据'], (self.segmentResult.steps || []).map(function(s) { return [s.step, status(s.state === 'ok' || !s.state && s.ok ? '已观测' : s.state === 'failed' ? '未完成' : s.state === 'skipped' ? '未测量' : '未知', s.state === 'ok' || !s.state && s.ok ? 'ok' : s.state === 'failed' ? 'warn' : 'idle'), s.text]; })) : note('尚未运行检查'),
                        self.segmentResult ? note('检查于 ' + new Date(self.segmentResult.ts_ms).toLocaleString() + ' · 用时 ' + self.segmentResult.ms + ' ms；结果仅对应本次连接，切换设备后清除。') : null].filter(Boolean))),
                    note('诊断码 ' + (d.code || '—') + ' · 未采集的数据以“—”显示。诊断包不含配对密钥、令牌与 Wi-Fi 密码。')];
            } else if (tab === 'info') {
                self.procData = r;
                var sys = r.system || {};
                content = [section('车机屏幕', card([row('浏览器', viewer.ua || '尚未连接'), row('窗口 · 画面区', win.w ? win.w + '×' + win.h + ' · ' + win.rw + '×' + win.rh : '—'), row('解码', car.decoder || '—'), row('到 F50 延迟 · 抖动', val(car.rtt_ms, ' ms') + ' · ' + val(car.jitter_ms, ' ms'))])),
                    section('F50', card([row('系统', [sys.openwrt, sys.kernel].filter(Boolean).join(' · ') || '—'), row('上行', f.uplink || '—'), row('Wi‑Fi', [self.data.network && self.data.network.ssid, f.wifi && f.wifi.channel ? 'CH' + f.wifi.channel : null, f.wifi && f.wifi.clients != null ? f.wifi.clients + ' 台客户端' : null].filter(Boolean).join(' · ') || '—'), row('蓝牙', sys.bluez || '—')])),
                    section('iPhone', card([row('名称', (self.data.phone || {}).name || '未连接'), row('型号 · 系统', [phone.model, phone.ios ? 'iOS ' + phone.ios : null].filter(Boolean).join(' · ') || '—'), row('Wi-Fi', val(phone.rssi, ' dBm') + ' · ' + val(phone.rate_mbps, ' Mbps')), row('请求画面', spec ? spec.width + '×' + spec.height + ' · ' + spec.fps + ' fps' : '—')]))];
            } else if (tab === 'timeline') {
                content = [section('事件记录', card([
                    E('div', {class: 'cw-actions'}, [['current', '当前会话'], ['all', '全部事件']].map(function(scope) { return E('button', {type: 'button', class: 'btn cbi-button ' + (self.timelineScope === scope[0] ? 'cbi-button-positive' : 'cbi-button-neutral'), 'aria-pressed': String(self.timelineScope === scope[0]), click: function() { self.timelineScope = scope[0]; self.loadDiag(); }}, scope[1]); })),
                    note(r.scope === 'all' ? '全部事件包括系统与其他设备；按时间正序显示。' : r.session ? '当前会话 ' + r.session.id + ' · ' + (r.session.phone || (self.data.phone || {}).name || 'iPhone') + '；只显示此会话期间的事件。' : '当前没有 CarPlay 会话；可切换“全部事件”查看连接尝试与系统记录。'),
                    self.rawOutput('timeline', (r.events || []).slice().reverse().map(function(e) { return new Date(e.ts_ms).toISOString() + ' ' + (e.kind || '') + ' ' + (e.msg || e.title || '') + (e.detail || e.cause ? ' ' + (e.detail || e.cause) : ''); }).join('\n') || '暂无事件')
                ]))];
            } else if (tab === 'logs') {
                self.logLines = (r.lines || []).slice().reverse().map(function(line) { return line.replace(/[\r\n]+$/, ''); });
                var entries = r.entries || [], modules = Array.from(new Set((self.logModules || []).concat(entries.map(function(e) { return e.module; })).filter(Boolean))).sort();
                self.logModules = modules;
                content = [section('运行日志', card([
                    E('div', {class: 'cw-actions'}, [
                        E('label', {}, ['级别 ', E('select', {'aria-label': '日志级别', change: function() { self.logLevel = this.value; self.loadDiag(); }}, [['', '全部'], ['error', '仅错误'], ['warn', '警告与错误'], ['info', '信息及以上']].map(function(o) { return E('option', {value: o[0], selected: self.logLevel === o[0] || null}, o[1]); }))]),
                        E('label', {}, ['来源 ', E('select', {'aria-label': '日志来源', change: function() { self.logModule = this.value; self.loadDiag(); }}, [['', '全部来源']].concat(modules.map(function(m) { return [m, m]; })).map(function(o) { return E('option', {value: o[0], selected: self.logModule === o[0] || null}, o[1]); }))]),
                        E('button', {type: 'button', class: 'btn cbi-button cbi-button-neutral', click: function() { cp.download('cowplay-filtered.log', self.logLines.join('\n')); }}, '下载当前日志')
                    ]),
                    note('按时间正序显示 · 本次显示 ' + entries.length + ' 条 / 匹配 ' + (r.matched == null ? r.total : r.matched) + ' 条；INFO 信息、WARN 警告、ERROR 错误、DEBUG 调试。导出使用相同筛选。'),
                    self.rawOutput('logs', self.logLines.join('\n') || '暂无日志')
                ].filter(Boolean)))];
            } else {
                self.procData = r;
                var proc = r.process || {};
                content = [section('进程', card([row('cowplay', proc.pid ? 'PID ' + proc.pid + ' · ' + val(proc.threads) + ' 线程 · RSS ' + val(proc.rss_kb, ' KB') + ' · CPU ' + val(proc.cpu_percent, '%') : '—'), row('程序', proc.binary || '—'), row('启动方式', proc.launcher || '—'), row('监听', (proc.ports || []).map(function(p) { return p.port + ' ' + p.use; }).join('；') || '—')])), section('当前 CowPlay 配置', card(E('pre', {class: 'cw-pre', 'data-scroll': 'config', tabindex: '0'}, r.config || '暂无配置')))];
            }
            self.replaceDiag(content);
        }).catch(function(e) { if (epoch === self.diagEpoch && !self.data.error && self.page === 'diag' && tab === self.diagTab) self.diagBody.replaceChildren(note('暂时无法读取此视图：' + e.message), E('button', {type: 'button', class: 'btn cbi-button cbi-button-neutral', click: function() { if (tab === 'checks') self.sampling = !self.sampling; self.loadDiag(); }}, tab === 'checks' && self.sampling ? '停止采样' : '重试')); }).finally(function() { if (self.diagPending && self.diagPending.promise === pending) self.diagPending = null; });
        this.diagPending = {key: key, promise: pending};
        return pending;
    },
    updateCert: function() {
        var c = this.certData || {}, net = this.data.network || {};
        var task = c.task || {};
        var info = [row('证书来源', net.cert_source === 'acme' ? '自动续期' : net.cert_source === 'upload' || net.cert_source === 'uploaded' ? '手动上传' : net.cert_source || '—'), row('当前证书', (c.subject || []).join(', ') || '—'), row('有效期', c.days == null ? '未测得' : status(c.valid === false ? '未生效或已过期' : c.days === 0 ? '有效 · 不足 1 天' : c.days > 0 ? '剩 ' + c.days + ' 天' : '已过期', c.days > 30 ? 'ok' : 'warn'))];
        if (c.error) info.push(row('证书状态', '读取失败：' + c.error));
        if (task.running || task.cert_ok === false) info.push(row('续期状态', task.running ? task.cert_progress || '处理中…' : task.cert_result || '续期失败'));
        this.certInfo.replaceChildren.apply(this.certInfo, info);
        this.certExtraInfo.replaceChildren(row('证书链', c.chain == null ? '—' : c.chain + ' 张证书'));
        this.renewInfo.replaceChildren(row('申请域名', c.domain || net.origin || '—'), row('DNS 验证', (c.dns_provider || '未知') + ' · ' + (c.token_set ? '凭据已保存' : '未配置凭据')), row('续期任务', task.running ? task.cert_progress || '处理中…' : task.cert_result || '尚未运行'), row('最近续期', task.cert_ms ? new Date(task.cert_ms).toLocaleString() + ' · ' + (task.cert_ok === true ? '成功' : task.cert_ok === false ? '失败' : '未知') : '—'));
        this.renewButton.disabled = !!task.running || !L.hasViewPermission();
        this.rollbackButton.disabled = !c.rollback || !L.hasViewPermission();
        if (this.updateCertificateUpload) this.updateCertificateUpload();
    },
    setEditState: function(state) {
        this.editState = state;
        this.updateFooter();
    },
    updateFooter: function() {
        if (this.restartRequired && Number.isInteger(this.restartPid) && Number.isInteger((this.data || {}).pid) && this.data.pid !== this.restartPid) this.restartRequired = false;
        if (this.saveNote) this.saveNote.textContent = ({clean: '没有未应用的更改', dirty: '有更改尚未保存', saved: '已保存，尚未应用', applying: '正在应用配置…', reload_failed: '配置已提交，CowPlay 重载失败；请修正后重新应用', applied: '✓ 配置已应用'}[this.editState] || '') + (this.restartRequired ? '；地址或热点变更需要重启 CowPlay。' : '');
        if (this.footer) this.footer.hidden = !this.restartRequired && (this.page === 'overview' || this.page === 'diag') && (this.editState === 'clean' || this.editState === 'applied');
        if (this.footer) this.footer.querySelectorAll('button').forEach(function(b) { b.disabled = !!this.updateBusy || !L.hasViewPermission() || !!this.importRunning || !!this.applyPending || this.editState === 'applying'; }, this);
    },
    render: function(data) {
        var self = this; this.data = data[3]; this.phones = data[4]; this.screens = data[5]; this.certData = data[6]; this.procData = data[7]; this.serviceData = data[8];
        this.root = E('div', {class: 'cw'}); this.intro = E('p', {class: 'cw-intro'}); this.error = E('p', {class: 'alert-message warning cw-error', role: 'status'}); this.pill = E('span', {class: 'cw-pill'}); this.wirelessNotice = E('p', {class: 'alert-message warning cw-error', role: 'status'}); this.version = E('small');
        this.overview = E('div'); this.phoneTable = E('div'); this.screenTable = E('div'); this.runtime = E('div'); this.about = E('div'); this.diagBody = E('div', {'aria-live': 'polite'}); this.certInfo = E('div'); this.certExtraInfo = E('div'); this.renewInfo = E('div'); this.portStatus = E('p', {class: 'cw-foot', role: 'status'}); this.lanAddress = E('span'); this.runAddress = E('span');
        var mfiCert = '', mfiKey = '', mfiSending = false, mfiVersions = {certificate: 0, private_key: 0};
        var mfiNotice = E('p', {class: 'cw-foot', role: 'status'});
        var mfiButton = this.action('验证并导入 CarPlay 身份', 'mfi_upload', function() { mfiSending = true; mfiInputs.forEach(function(e) { e.disabled = true; }); return [mfiCert, mfiKey]; }, true, function() {
            mfiSending = false; mfiInputs.forEach(function(e) { e.disabled = !L.hasViewPermission(); });
            mfiCert = ''; mfiKey = ''; mfiInputs.forEach(function(e) { e.value = ''; });
            mfiNotice.textContent = '已保存。请在系统页手动重启 CowPlay 后生效；配对记录保留。';
        }, function() { return !mfiSending && !!mfiCert && !!mfiKey; }, null, function(result) { mfiSending = false; mfiInputs.forEach(function(e) { e.disabled = !L.hasViewPermission(); if (result) e.value = ''; }); if (result) mfiCert = mfiKey = ''; });
        function identityFile(label, field, pemLabel, accept) {
            return E('input', {type: 'file', accept: accept, 'aria-label': label, disabled: !L.hasViewPermission() || null, change: function() {
                var version = ++mfiVersions[field], file = this.files[0];
                if (field === 'certificate') mfiCert = ''; else mfiKey = '';
                mfiButton.disarm(); mfiButton.disabled = true; mfiNotice.textContent = '';
                if (!file) return;
                if (!file.size || file.size > 16384) { mfiNotice.textContent = '每个 CarPlay 文件须为 1–16384 字节。'; return; }
                var reader = new FileReader();
                reader.onload = function() {
                    if (version !== mfiVersions[field]) return;
                    var bytes = new Uint8Array(reader.result);
                    if (bytes[0] !== 0x30) { mfiNotice.textContent = '请选择二进制 DER / P7B 文件；私钥使用 PKCS8 DER。'; return; }
                    var encoded = btoa(String.fromCharCode.apply(null, bytes));
                    var pem = '-----BEGIN ' + pemLabel + '-----\n' + encoded.match(/.{1,64}/g).join('\n') + '\n-----END ' + pemLabel + '-----\n';
                    if (field === 'certificate') mfiCert = pem; else mfiKey = pem;
                    mfiButton.disabled = !mfiCert || !mfiKey || !L.hasViewPermission();
                    mfiNotice.textContent = '文件已读取，导入时校验证书与私钥匹配。';
                };
                reader.onerror = function() { if (version === mfiVersions[field]) mfiNotice.textContent = '文件读取失败，请重新选择。'; };
                reader.readAsArrayBuffer(file);
            }});
        }
        var mfiInputs = [identityFile('CarPlay 证书 P7B / DER', 'certificate', 'PKCS7', '.p7b,.der'), identityFile('CarPlay 私钥 PKCS8 DER', 'private_key', 'PRIVATE KEY', '.pk8,.der')];
        this.mfiStatus = E('p', {class: 'cw-foot'});
        this.mfiSection = section('CarPlay 配件身份（MFi）', card([
            note('导入你从第三方合法取得的配件证书及匹配私钥。项目不提供此资料；与下方浏览器 HTTPS 证书分别配置。'),
            row('证书 P7B / X.509 DER', mfiInputs[0]), row('P-256 私钥 PKCS8 DER', mfiInputs[1]),
            E('div', {class: 'cw-actions'}, mfiButton), mfiNotice, this.mfiStatus,
            note('验证通过后成对保存，原文件保留。手动重启 CowPlay 后生效，重启会中断当前投屏。')
        ]));
        var token = E('code', {}, '••••••'), tokenEpoch = 0, showEpoch = 0, resetEpoch = 0, tokenTimer;
        var hideToken = function() { tokenEpoch++; clearTimeout(tokenTimer); token.textContent = '••••••'; };
        var revealToken = function(result, epoch) { if (epoch !== tokenEpoch) return; token.textContent = result.token; clearTimeout(tokenTimer); tokenTimer = setTimeout(hideToken, 30000); };
        this.hideManagementToken = hideToken;
        document.addEventListener('visibilitychange', function() { if (document.hidden) hideToken(); });
        this.tokenDisplay = token;
        this.tokenActions = E('div', {class: 'cw-actions'}, [this.action('显示令牌', 'token', function() { hideToken(); showEpoch = tokenEpoch; return []; }, false, function(r) { revealToken(r, showEpoch); }), E('button', {type: 'button', class: 'btn cbi-button cbi-button-neutral', click: hideToken}, '隐藏令牌'), this.action('重置令牌', 'token_reset', function() { hideToken(); resetEpoch = tokenEpoch; return []; }, true, function(r) { revealToken(r, resetEpoch); })]);
        var tokenField = E('input', {type: 'password', autocomplete: 'new-password', placeholder: '只写入，不回显', 'aria-label': '证书 DNS API 令牌'});
        this.dnsToken = tokenField;
        var dnsSnapshot = '';
        this.dnsTokenSave = this.action('保存证书 DNS 凭据', 'ddns_token', function() { dnsSnapshot = tokenField.value; tokenField.disabled = true; return [dnsSnapshot]; }, false, function() { if (tokenField.value === dnsSnapshot) tokenField.value = ''; }, null, null, function() { tokenField.disabled = !L.hasViewPermission(); });
        var fileNotice = E('p', {class: 'cw-foot cw-cert-notice', role: 'status'}), certFields = {}, certBusy = false, inputProblem;
        this.uploadCert = ''; this.uploadKey = '';
        var inputError = function() {
            if (!L.hasViewPermission()) return '无写入权限，请使用管理员账号。';
            if (((self.certData || {}).task || {}).running) return '证书任务正在进行。';
            if (Object.keys(certFields).some(function(k) { return certFields[k].reading; })) return '正在读取文件…';
            if (!self.uploadCert) return '粘贴或导入证书链 PEM。';
            if (!self.uploadKey) return '补充匹配的私钥 PEM。';
            if (new TextEncoder().encode(JSON.stringify({cert: self.uploadCert, key: self.uploadKey})).length > 49152) return '证书链与私钥合计须不超过 48 KiB。';
            if (self.uploadCert.indexOf('-----BEGIN CERTIFICATE-----') < 0 || self.uploadCert.indexOf('-----END CERTIFICATE-----') < 0) return '证书链须为完整 PEM，含 BEGIN / END CERTIFICATE。';
            if (/-----BEGIN ENCRYPTED PRIVATE KEY-----|Proc-Type:\s*4,ENCRYPTED/i.test(self.uploadKey)) return '私钥已加密，请先在本地解密。';
            if (!/-----BEGIN (RSA |EC )?PRIVATE KEY-----[\s\S]+?-----END \1PRIVATE KEY-----/.test(self.uploadKey)) return '私钥须为完整、未加密的 PEM。';
            return '';
        };
        var ready = function() { return !certBusy && !inputError(); };
        var updateInput = function(message) { inputProblem = inputError(); certBtn.disabled = !ready(); fileNotice.textContent = message || inputProblem || '已就绪，可验证并替换。'; };
        var setCertBusy = function(busy) { certBusy = busy; Object.keys(certFields).forEach(function(k) { var f = certFields[k]; f.text.disabled = f.file.disabled = busy || !L.hasViewPermission(); }); };
        var certBtn = this.action('验证并替换证书', 'cert_upload', function() {
            setCertBusy(true); fileNotice.textContent = '正在验证并替换…'; return [self.uploadCert, self.uploadKey];
        }, true, function() {
            Object.keys(certFields).forEach(function(k) { var f = certFields[k]; self[k] = f.text.value = f.file.value = ''; });
            setCertBusy(false); updateInput('证书已替换并立即生效，输入已清除。');
        }, ready, function(e) { if (certBusy) { setCertBusy(false); updateInput('替换未完成：' + e.message); } }, function(result) { setCertBusy(false); if (result) Object.keys(certFields).forEach(function(k) { var f = certFields[k]; self[k] = f.text.value = f.file.value = ''; }); });
        this.updateCertificateUpload = function() { certBtn.disabled = !ready(); if (inputError() !== inputProblem) updateInput(); };
        var pemEditor = function(which, label, accept) {
            var f = certFields[which] = {sequence: 0, reading: false,
                text: E('textarea', {rows: 5, wrap: 'off', autocomplete: 'off', autocapitalize: 'off', spellcheck: 'false', 'aria-label': label + ' PEM 内容', placeholder: '粘贴 PEM，或选择文件导入'}),
                file: E('input', {type: 'file', accept: accept, 'aria-label': label + ' PEM'})};
            var cancelRead = function() { f.sequence++; f.reading = false; if (f.reader) { f.reader.abort(); f.reader = null; } certBtn.disarm(); };
            var readText = function() { self[which] = f.text.value.trim().replace(/\r\n?/g, '\n'); updateInput(); };
            f.text.addEventListener('input', function() { cancelRead(); f.file.value = ''; readText(); });
            f.file.addEventListener('change', function() {
                cancelRead(); self[which] = f.text.value = ''; var sequence = f.sequence, file = f.file.files[0];
                if (!file) { updateInput(); return; }
                if (file.size > 49152) { updateInput('文件过大，证书与私钥合计限 48 KiB。'); return; }
                f.reading = true; updateInput();
                var reader = f.reader = new FileReader();
                reader.onload = function() { if (f.sequence !== sequence) return; f.reading = false; f.reader = null; f.text.value = String(reader.result || ''); readText(); };
                reader.onerror = reader.onabort = function() { if (f.sequence !== sequence) return; f.reading = false; f.reader = null; updateInput('无法读取' + label + '文件，可重选或粘贴。'); };
                try { reader.readAsText(file, 'UTF-8'); } catch (e) { reader.onerror(); }
            });
            return E('div', {class: 'cw-pem-editor'}, [f.text, f.file]);
        };
        var certEditor = pemEditor('uploadCert', '证书链', '.pem,.crt,.cer'), keyEditor = pemEditor('uploadKey', '私钥', '.pem,.key');
        setCertBusy(false); updateInput();
        this.rollbackButton = this.action('回滚到上一张证书', 'cert_rollback', [], true, null, function() { return !!self.certData.rollback && !(self.certData.task || {}).running; });
        this.renewButton = this.action('申请 / 续期', 'cert_renew', [], false, null, function() { return !(self.certData.task || {}).running; });
        this.certSettingsBody = E('div');
        var certSection = section('HTTPS 证书', card([this.certInfo, row('证书链 PEM', certEditor), row('私钥 PEM', keyEditor), fileNotice, row('操作', E('div', {class: 'cw-actions'}, [certBtn, this.rollbackButton])), E('details', {'data-cert-details': ''}, [E('summary', {}, '证书要求'), this.certExtraInfo,
            note('使用浏览器信任的完整证书链，域名和私钥须匹配。手动替换后停止自动续期。')])]));
        this.renewSection = section('证书申请与续期', E('details', {'data-certificate-renewal': ''}, [E('summary', {}, '自动申请 / 续期（可选）'), card([
            note('DNS-01 用于验证域名并签发证书，手动上传无需配置。'), this.renewInfo, this.certSettingsBody, this.renewButton,
            E('a', {href: 'https://letsencrypt.org/docs/challenge-types/', target: '_blank', rel: 'noopener'}, 'DNS 验证说明 ↗')])]));
        var backupInput = E('input', {type: 'file', 'aria-label': '导入 CowPlay 配置'}), importNotice = note('请选择不超过 32 KB 的 CowPlay 配置文件。'), importSequence = 0, importReading = false, importModal, importProgress;
        this.importText = '';
        var restore = this.action('恢复配置', 'config_import', function() {
            backupInput.disabled = true; self.importRunning = true; self.root.inert = true; self.updateFooter();
            importProgress = E('p', {class: 'spinning', role: 'status', 'data-cw-import-progress': ''}, '正在恢复配置，请稍候。完成后会刷新页面并读取恢复后的设置。');
            importModal = ui.showModal('正在恢复 CowPlay 配置', [importProgress]);
            return [self.importText];
        }, true, null, function() { return !importReading && !!self.importText; }, null, function(result) {
            // Keep the successful import locked until its page reload replaces the old form cache.
            if (result) return;
            self.importRunning = false; self.root.inert = false; backupInput.disabled = !L.hasViewPermission(); self.updateFooter();
            if (importModal && importModal.contains(importProgress)) ui.hideModal();
        });
        backupInput.addEventListener('change', function() {
            if (backupInput.disabled) return;
            var sequence = ++importSequence, file = backupInput.files[0]; self.importText = ''; importReading = false; restore.disarm(); restore.syncPending();
            if (!file) { importNotice.textContent = '尚未选择配置文件。'; return; }
            if (!file.size || file.size > 32768) { importNotice.textContent = '配置文件须为 1 字节至 32 KB；未保留之前的文件。'; return; }
            importReading = true; restore.syncPending(); importNotice.textContent = '正在读取配置文件…';
            var reader = new FileReader();
            reader.onload = function() { if (sequence !== importSequence) return; importReading = false; self.importText = String(reader.result || ''); importNotice.textContent = self.importText ? '已读取 ' + file.name + '，恢复前将再次确认。' : '配置文件为空，请重新选择。'; restore.syncPending(); };
            reader.onerror = reader.onabort = function() { if (sequence !== importSequence) return; importReading = false; importNotice.textContent = '配置文件读取失败，请重新选择。'; restore.syncPending(); };
            try { reader.readAsText(file, 'UTF-8'); } catch (e) { reader.onerror(); }
        });
        return Promise.all([this.devicesForm(), this.displayForm(), this.networkForm(), this.systemForm()]).then(function(forms) {
            self.formNodes = forms; self.certSection = certSection; self.screensSection = section('已授权的车机屏幕', self.screenTable); self.placeSections();
            var panes = {
                overview: self.overview,
                devices: E('div', {}, [section('已保存的 iPhone', self.phoneTable), forms[0]]),
                display: E('div', {}, [section('iPhone 能力与当前会话',self.displaySupport=E('div',{class:'cw-card'})),forms[1],note('接收端声明范围不是 iPhone 能力表；实际由手机决定是否接受。车机“控制”可边看边调，H.265 需显式选择且仍属实验。控制栏和网络属于本机设置。')]),
                network: E('div', {}, [forms[2], self.mfiSection, self.wirelessInfo(), self.ddnsPanel = E('div', {}, self.ddnsInfo())]),
                diag: self.diagBody,
                system: E('div', {}, [section('服务', self.runtime), section('配件身份', card(row('资料来源', '读取设备已有身份；不随软件包分发'))), forms[3], section('备份与恢复', card([row('CowPlay 配置（不含配件身份）', self.action('下载备份', 'config_export', [], false, function(r) { cp.download('cowplay.uci', r.text); })), row('导入配置', backupInput), importNotice, row('操作', E('div', {class: 'cw-actions'}, [restore])), note('恢复操作会替换 CowPlay 配置并刷新页面，未保存的编辑将丢弃；不修改配对和配件身份。')])), section('GitHub 安装与升级', updater.render(function() { return self.page === 'system'; }, function(busy) { self.updateBusy = busy; if (self.root) self.syncActions(); self.updateFooter(); }, function() { return !self.importRunning && !self.applyPending && ['dirty', 'saved', 'applying', 'reload_failed'].indexOf(self.editState) < 0; })), section('关于', self.about)])
            };
            self.root.append(E('link', {rel: 'stylesheet', href: L.resource('cowplay/style.css')}), E('div', {class: 'cw-head'}, [E('div', {class: 'cw-grow'}, [E('h2', {}, 'CowPlay'), self.version]), self.pill]), E('ul', {class: 'cbi-tabmenu cw-tabs', role: 'tablist', 'aria-label': 'CowPlay 分组'}, tabs.map(function(t) { return E('li', {}, E('a', {href: '#' + t[0], role: 'tab', 'aria-label': t[1], 'data-target': t[0], 'data-diag': diagnosticTabs.some(function(d) { return d[0] === t[0]; }) ? t[0] : null, click: function(ev) { ev.preventDefault(); self.select(t[0]); }}, t[1])); })), self.intro, self.error);
            Object.keys(panes).forEach(function(page) { self.root.append(E('div', {'data-page': page, role: 'tabpanel'}, panes[page])); });
            self.update(); self.select(self.page === 'diag' ? self.diagTab : self.page);
            var markDirty = function(ev) { if (ev.target.closest('.cbi-map') && !ev.target.closest('.cw-pem-editor')) { self.formEpoch++; confirmation.cancel(); self.setEditState('dirty'); } };
            self.root.addEventListener('input', markDirty);
            self.root.addEventListener('change', markDirty);
            uci.changes().then(function(changes) { if (Object.keys(changes).length && self.editState === 'clean') self.setEditState('saved'); });
            poll.add(function() { return self.refresh(); }, 2);
            setTimeout(function() { network.inspect(self, Number(self.portOption.formvalue('main'))); network.resume(self).catch(cp.failure); }, 0);
            return self.root;
        });
    },
    placeSections: function() {
        // Reinsert read-only sections after LuCI redraws a saved/reset map.
        this.certSection.remove(); this.renewSection.remove(); this.screensSection.remove();
        var access = this.formNodes[0].querySelectorAll('.cbi-section')[1], dns = this.formNodes[2].querySelectorAll('.cbi-section')[1];
        if (dns) this.certSettingsBody.replaceChildren(dns);
        this.formNodes[2].append(this.certSection);
        this.formNodes[2].append(this.renewSection);
        var portRow=this.portStatus.closest('.cbi-value');if(portRow)portRow.hidden=!this.portStatus.textContent;
        if (access) access.before(this.screensSection);
        this.formNodes[1].querySelectorAll('select[id$=".max_fps"] option[value=""]').forEach(function(o) { if (o.parentElement.value) o.remove(); else o.disabled = true; });
    },
    handleSave: function(networkOwner) {
        if(this.importRunning)return Promise.reject(new Error('配置正在恢复，请等待页面刷新'));
        if(this.updateBusy)return Promise.reject(new Error('软件更新任务尚未结束，请先检查任务状态'));
        if (this.networkBusy && (!networkOwner || networkOwner !== this.applyPending)) return Promise.reject(new Error('端口接管正在进行'));
        var self = this, ddnsBefore = null, persisted = false, maps = this.maps;
        // All maps share LuCI's UCI cache. Parse all inputs before one save, then refresh them.
        maps.forEach(function(m) { m.checkDepends(); });
        return Promise.all(maps.map(function(m) { return m.parse(); }))
            .then(function() { return network.inspect(self, Number(self.portOption.formvalue('main'))); })
            .then(function() {
                if (self.disableDdnsPending) {
                    ddnsBefore = {ddns_enabled: uci.get('cowplay', 'main', 'ddns_enabled'), ddns_mode: uci.get('cowplay', 'main', 'ddns_mode')};
                    uci.set('cowplay', 'main', 'ddns_enabled', '0'); uci.set('cowplay', 'main', 'ddns_mode', 'luci');
                }
                return uci.save();
            })
            .then(function() { persisted = true; self.disableDdnsPending = false; })
            .then(function() { return Promise.all(maps.map(function(m) { return m.load().then(function() { return m.renderContents(); }); })); })
            .then(L.bind(function() { this.placeSections(); this.setEditState('saved'); this.ddnsPanel.replaceChildren(this.ddnsInfo()); return ui.changes.init(); }, this)).finally(function() { if (!persisted && ddnsBefore) Object.keys(ddnsBefore).forEach(function(k) { uci.set('cowplay', 'main', k, ddnsBefore[k]); }); }).catch(function(e) { cp.failure(e); throw e; });
    },
    handleSaveApply: async function(ev, mode) {
        if (this.updateBusy || this.importRunning || this.applyPending || this.editState === 'applying') return;
        var self = this, token = {}, port = Number(this.portOption.formvalue('main')), context = this.actionContext(), formEpoch = this.formEpoch;
        var current = function() { return !self.updateBusy && self.applyPending === token && L.hasViewPermission() && self.actionContext() === context && self.formEpoch === formEpoch && Number(self.portOption.formvalue('main')) === port; };
        this.applyPending = token; this.updateFooter();
        try {
            if (!Number.isInteger(port) || port < 1 || port > 65535) throw new Error('HTTPS 端口须为 1–65535 的整数');
            var report = await network.inspect(this, port);
            if (Number(this.portOption.formvalue('main')) !== port) throw new Error('端口检查已过期，请重新确认');
            if (!current()) return;
            if (!report || report.available == null) throw new Error('需恢复端口检查后才能应用；更改尚未保存');
            var save = async function() {
                if (!current()) throw new Error('设置或页面已变化，请重新确认');
                await self.handleSave(token);
                if (!current()) throw new Error('配置已保存，页面或连接已变化，请重新应用');
                if (Number(self.portOption.formvalue('main')) !== port || Number(uci.get('cowplay', 'main', 'port')) !== port) throw new Error('端口已变化，请重新确认');
                return network.inspect(self, port);
            };
            if (report.available === false) { await network.apply(this, report, save); return; }
            if (!await confirmation.ask({key: 'apply', title: '保存并应用所有设置？', description: '配置对所有屏幕共用。应用时 CarPlay 可能断开后重新连接，画面和声音会短暂中断，配对保留。地址或热点相关更改还需手动重启 CowPlay。', confirm: '保存并应用', current: current}) || !current()) return;
            report = await save();
            if (!report || report.available !== true) throw new Error('配置已保存，端口占用已变化，请重新确认应用');
            // LuCI apply returns immediately; only uci-applied proves that commit/connectivity confirmation succeeded.
            if (self.applyListener) document.removeEventListener('uci-applied', self.applyListener);
            self.applyListener = function() {
                cp.call('reload').then(function(r) {
                    self.restartRequired = !!r.restart_required; self.restartPid = self.data.pid; self.updateFooter();
                    self.setEditState('applied');
                    return self.refresh();
                }).catch(function(e) { self.setEditState('reload_failed'); cp.failure(e); });
            };
            document.addEventListener('uci-applied', self.applyListener, {once: true});
            self.setEditState('applying');
            await ui.changes.apply(mode === '0');
        } catch (e) { if (self.editState === 'applying') { document.removeEventListener('uci-applied', self.applyListener); self.setEditState('saved'); } cp.failure(e); }
        finally { if (this.applyPending === token) this.applyPending = null; this.updateFooter(); }
    },
    handleReset: function() { if(this.importRunning)return Promise.resolve(); var self = this; this.disableDdnsPending = false; return Promise.all(this.maps.map(function(m) { return m.reset(); })).then(function() { self.placeSections(); self.ddnsPanel.replaceChildren(self.ddnsInfo()); return uci.changes(); }).then(function(changes) { self.setEditState(Object.keys(changes).length ? 'saved' : 'clean'); }); },
    addFooter: function() {
        var self = this, disabled = !L.hasViewPermission() || null;
        this.saveNote = E('span', {class: 'cw-foot cw-grow', role: 'status'});
        this.setEditState(this.editState);
        this.footer = E('div', {class: 'cbi-page-actions cw-footer'}, [this.saveNote,
            E('button', {type: 'button', class: 'btn cbi-button cbi-button-reset cw-btn', disabled: disabled, click: ui.createHandlerFn(this, 'handleReset')}, '复位'),
            E('button', {type: 'button', class: 'btn cbi-button cbi-button-save cw-btn', disabled: disabled, click: ui.createHandlerFn(this, 'handleSave')}, '保存'),
            E('button', {type: 'button', class: 'btn cbi-button cbi-button-apply cw-btn', disabled: disabled, click: ui.createHandlerFn(this, function(ev) { return self.handleSaveApply(ev, '0'); })}, '保存并应用')]);
        this.updateFooter();
        return this.footer;
    }
});
