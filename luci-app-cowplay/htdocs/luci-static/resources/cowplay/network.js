'use strict';
'require ui';
'require uci';
'require cowplay.rpc as cp';
'require cowplay.confirmation as confirmation';

function currentPort() { return Number(location.port || (location.protocol === 'https:' ? 443 : 80)); }
function owners(report) { return (report.owners || []).map(function(o) { return (o.service === 'uhttpd' ? 'LuCI / uhttpd' : o.service || o.process || '未知') + (o.pid ? '（PID ' + o.pid + '）' : '') + ' · ' + (o.addresses || []).join(', '); }); }
function wait(ms) { return new Promise(function(resolve) { setTimeout(resolve, ms); }); }
async function applyConfiguration() {
    // Older device LuCI resolves uci.apply() before its delayed confirmation.
    // Confirm explicitly while this management origin is still available.
    var result = await uci.callApply(10, true);
    if (result !== 0) throw new Error('配置应用失败');
    var until = performance.now() + 10000;
    await wait(1000);
    while (performance.now() < until) {
        result = await uci.callConfirm();
        if (result === 0) return;
        await wait(250);
    }
    throw new Error('配置确认超时，未接管端口');
}
function jobUrl(job, base) {
    var url = new URL(base || location.href);
    if (url.protocol === 'https:' && job.management_scheme !== 'https') throw new Error('不会将 HTTPS 管理入口降为 HTTP');
    url.protocol = job.management_scheme + ':'; url.port = String(job.management_port);
    url.searchParams.set('cowplay_network_job', job.id); url.searchParams.set('cowplay_network_return_port', String(job.original_management_port)); url.hash = 'network'; return url;
}
function confirm(view, report) {
    var stopped = Array.from(new Set((report.owners || []).map(function(o){return o.service}).filter(function(s){return s && s!=='uhttpd'})));
    var actions = (report.management_port ? 'LuCI HTTPS 使用备用端口 ' + report.management_port + '；确认管理入口可用后释放 ' + report.port + '。' : '') + (stopped.length ? '停止服务 ' + stopped.join('、') + '。' : '');
    var context = view.actionContext(), formEpoch = view.formEpoch;
    var current = function() { return L.hasViewPermission() && view.actionContext() === context && view.formEpoch === formEpoch && Number(view.portOption.formvalue('main')) === report.port; };
    return confirmation.ask({key: 'network-apply', title: '覆盖端口 ' + report.port, description: owners(report).join('；') + '。' + actions + '确认后保存并应用所有设置，随后重启 CowPlay，当前投屏会断开；配对保留。', confirm: '覆盖并应用', current: current}).then(function(approved) { return approved && current(); });
}
function progress(message) { ui.showModal('端口接管', [E('p', {class: 'spinning', role: 'status'}, message)]); }
function problem(view, job) {
    var buttons = [E('button', {class: 'btn', click: function() {
        ui.hideModal();
        if (['rolled_back','cancelled','failed'].indexOf(job.phase)>=0) { var url=new URL(location.href);url.searchParams.delete('cowplay_network_job');url.searchParams.delete('cowplay_network_return_port');history.replaceState(null,'',url); }
    }}, '关闭')];
    if (job.phase !== 'rolled_back' && L.hasViewPermission()) buttons.push(E('button', {class: 'btn cbi-button-negative', click: function() {
        cp.call('network_recover', job.id).then(function() { return monitor(view, job.id); }).catch(cp.failure);
    }}, '恢复原配置'));
    ui.showModal('接管未完成', [E('p', {}, job.message), E('div', {class: 'cw-actions'}, buttons)]);
}
async function monitor(view, id) {
    var until = performance.now() + 200000, confirmed = false, last, failures = 0;
    view.networkBusy = true;
    try {
        while (performance.now() < until) {
            try { last = await cp.call('network_status', id); }
            catch (e) {
                if (++failures >= 3) {
                    var oldPort = Number(last && last.original_management_port || new URL(location.href).searchParams.get('cowplay_network_return_port'));
                    if (Number.isInteger(oldPort) && oldPort > 0 && oldPort <= 65535 && oldPort !== currentPort()) {
                        var previous = new URL(location.href); previous.port = String(oldPort);
                        try { await ui.pingDevice(location.protocol.slice(0,-1), previous.host); location.assign(previous.href); return {phase:'redirecting'}; } catch (ignored) {}
                    }
                }
                await wait(500); continue;
            }
            failures = 0;
            if (last.phase === 'complete') {
                ui.hideModal(); view.setEditState('applied');
                var clean = new URL(location.href); clean.searchParams.delete('cowplay_network_job'); clean.searchParams.delete('cowplay_network_return_port'); history.replaceState(null, '', clean);
                ui.addNotification(null, E('p', {}, last.message), 'info'); await view.refresh(); await inspectPort(view,last.port); await ui.changes.init(); return last;
            }
            if (['rolled_back', 'recovery_required', 'failed', 'cancelled'].indexOf(last.phase) >= 0) { problem(view, last); return last; }
            progress(last.message || '正在接管…');
            if (last.phase === 'prepared') { problem(view, {id:id,phase:'recovery_required',message:'接管尚未开始，可撤销未完成的端口配置。'}); return last; }
            if (last.phase === 'await_management') {
                var target = jobUrl(last);
                if (target.origin !== location.origin) {
                    try { await ui.pingDevice(last.management_scheme, target.host); location.assign(target.href); return {phase: 'redirecting'}; }
                    catch (e) { await wait(500); continue; }
                }
                if (!confirmed) {
                    try { await cp.call('network_confirm', id, location.protocol.slice(0,-1), currentPort()); confirmed = true; }
                    catch (e) { await wait(500); continue; }
                }
            }
            await wait(500);
        }
        problem(view, {id: id, phase: 'recovery_required', message: '接管状态暂时不可读，可恢复原配置。'});
        return last;
    } finally { view.networkBusy = false; }
}
function inspectPort(view, port) {
        var sequence = view.portCheckSequence = (view.portCheckSequence || 0) + 1;
        if (!Number.isInteger(port) || port < 1 || port > 65535) return Promise.resolve(null);
        return cp.call('network_check', port).then(function(report) {
            if (view.portCheckSequence !== sequence) return report;
            view.portReport = report;
            view.portStatus.textContent = report.available === false ? '端口 ' + port + ' 占用：' + (owners(report).join('；') || '未知进程') + (report.can_takeover ? '。可保存，应用时确认覆盖。' : '。可保存，暂不能自动接管。') + (report.issue ? ' '+report.issue : '') : report.issue || '';
            var row = view.portStatus.closest('.cbi-value'); if (row) row.hidden = !view.portStatus.textContent;
            return report;
        }).catch(function(e) {
            if (view.portCheckSequence !== sequence) return view.portReport;
            view.portReport = {port: port, available: null, issue: e.message};
            view.portStatus.textContent = '占用检查不可用：' + e.message + '；仍可保存。';
            var row = view.portStatus.closest('.cbi-value'); if (row) row.hidden = false;
            return view.portReport;
        });
}
return L.Class.extend({
    inspect: inspectPort,
    apply: async function(view, report, save) {
        if (view.networkBusy) throw new Error('端口接管正在进行');
        if (!report.can_takeover) throw new Error((save ? '更改尚未保存；' : '配置尚未应用；') + '未识别可自动恢复的占用服务，无法自动接管端口。请选择其他端口，或返回仅保存配置。');
        view.networkBusy = true; var job;
        try {
            if (!await confirm(view, report)) return;
            var checked = save ? await save() : await inspectPort(view, report.port);
            if (!checked || checked.port !== report.port || checked.available !== false || checked.fingerprint !== report.fingerprint) throw new Error('端口占用已变化，请重新确认应用');
            if(Number(view.portOption.formvalue('main'))!==report.port||Number(uci.get('cowplay','main','port'))!==report.port)throw new Error('端口已修改，请重新保存后应用');
            job = await cp.call('network_prepare', report.port, report.fingerprint, location.protocol.slice(0,-1), currentPort());
            view.setEditState('applying'); progress('正在应用配置…');
            try { await applyConfiguration(); }
            catch (error) { await cp.call('network_cancel', job.id).catch(function() {}); ui.hideModal(); throw new Error('配置应用失败，未接管端口'); }
            var url = new URL(location.href); url.searchParams.set('cowplay_network_job', job.id); history.replaceState(null, '', url);
            try { await cp.call('network_commit', job.id); }
            catch (e) {
                var state = await cp.call('network_status',job.id).catch(function(){return null});
                if (state && state.phase === 'prepared') { problem(view,{id:job.id,phase:'recovery_required',message:e.message}); return state; }
            }
            return await monitor(view, job.id);
        } finally { view.networkBusy = false; }
    },
    resume: function(view) {
        var id = new URL(location.href).searchParams.get('cowplay_network_job');
        if (id && /^[A-Za-z0-9_-]{6,64}$/.test(id)) return monitor(view, id);
        return cp.call('network_status', '').then(function(job) {
            if (job.phase === 'idle') return;
            if (job.phase === 'prepared') {
                ui.addNotification(null, E('div', {}, [E('p', {}, '端口接管未完成。'), E('button', {class:'btn',disabled:!L.hasViewPermission()||null,click:function(){cp.call('network_cancel',job.id).then(function(){location.reload()}).catch(cp.failure)}},'撤销端口变更')]), 'info'); return;
            }
            return monitor(view,job.id);
        }).catch(function() {});
    },
    managementURL: function(job, base) { return jobUrl(job, base).href; }
});
