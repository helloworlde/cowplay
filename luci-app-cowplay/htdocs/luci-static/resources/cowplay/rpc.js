'use strict';
'require rpc';
'require ui';

var schema = {
    update_status: [], update_check: ['tag'], update_apply: ['id'], update_prepare: ['id'], update_acknowledge: ['id'],
    status: [], diag: [], phones: [], screens: [], logs: ['level', 'limit', 'module'], timeline: ['scope'], proc: [],
    token: [], token_reset: [], cert: [], config_export: [], cmd: ['cmd', 'id'], client_remove: ['id'],
    reload: [], selftest: [], bundle: [], service_info: [], network_check: ['port'], network_prepare: ['port', 'fingerprint', 'management_scheme', 'management_port'], network_commit: ['id'], network_status: ['id'], network_confirm: ['id','management_scheme','management_port'], network_cancel: ['id'], network_recover: ['id'], ddns_sync: [], ddns_token: ['token'],
    mfi_upload: ['certificate', 'private_key'], cert_upload: ['cert', 'key'], cert_rollback: [], cert_renew: [], config_import: ['text'], factory_reset: [], service: ['action']
};
var calls = {};
Object.keys(schema).forEach(function(method) {
    calls[method] = rpc.declare({object: 'cowplay', method: method, params: schema[method], reject: true,
        expect: {'': {error: 'CowPlay 没有返回有效结果，请检查服务和 LuCI 权限'}}});
});
return L.Class.extend({
    call: function(method) {
        var args = Array.prototype.slice.call(arguments, 1);
        return calls[method].apply(null, args).then(function(result) {
            if (result.error) throw new Error(result.error);
            if (!Object.keys(result).length || method === 'diag' && (!Array.isArray((result.diag || {}).checks) || !Number.isFinite((result.stats || {}).ts_ms))
                || method === 'selftest' && !Array.isArray(result.steps)
                || method === 'bundle' && result.kind !== 'cowplay-diagnostics') throw new Error('CowPlay 返回的数据不完整，请刷新页面并核对核心与 LuCI 版本');
            return result;
        });
    },
    download: function(name, value) {
        var url = URL.createObjectURL(new Blob([typeof value === 'string' ? value : JSON.stringify(value, null, 2)], {type: typeof value === 'string' ? 'text/plain;charset=utf-8' : 'application/json'}));
        var a = E('a', {href: url, download: name}); document.body.appendChild(a); a.click(); a.remove();
        setTimeout(function() { URL.revokeObjectURL(url); }, 1000);
    },
    failure: function(e) { ui.addNotification(null, E('p', {}, '操作未完成：' + e.message), 'error'); }
});
