'use strict';

var diagnosticTabs = [['checks', '实时数据'], ['info', '设备信息'], ['timeline', '时间线'], ['logs', '日志'], ['proc', '进程与配置']];
var tabs = [['overview', '概览'], ['devices', '设备与访问'], ['display', '投屏策略'], ['network', '网络与证书']].concat(diagnosticTabs, [['system', '系统']]);

return L.Class.extend({
    diagnosticTabs: diagnosticTabs,
    tabs: tabs,
    styles: function(host, tesla) {
        var version = encodeURIComponent(host && host.build || L.env.resource_version || 'cowplay-shell-20261113');
        return ['style.css'].concat(tesla ? ['tesla.css'] : []).map(function(name) {
            return E('link', {rel: 'stylesheet', href: L.resource('cowplay/' + name) + '?v=' + version});
        });
    },
    header: function(version, status) {
        return E('div', {class: 'cw-head'}, [E('div', {class: 'cw-grow'}, [E('h2', {}, 'CowPlay'), version]), status]);
    },
    navigation: function(current, select) {
        return E('ul', {class: 'cbi-tabmenu cw-tabs', role: 'tablist', 'aria-label': 'CowPlay 分组'}, tabs.concat([['tesla', 'Tesla 车辆']]).map(function(t) {
            var local = select && t[0] !== 'tesla';
            return E('li', {class: current === t[0] ? 'cbi-tab' : 'cbi-tab-disabled'}, E('a', {
                href: t[0] === 'tesla' ? L.url('admin/services/cowplay/tesla') : local ? '#' + t[0] : L.url('admin/services/cowplay') + '#' + t[0],
                role: 'tab', 'aria-label': t[1], 'aria-selected': String(current === t[0]), 'data-target': t[0],
                'data-diag': diagnosticTabs.some(function(d) { return d[0] === t[0]; }) ? t[0] : null,
                click: local ? function(ev) { ev.preventDefault(); select(t[0]); } : null
            }, t[1]));
        }));
    }
});
