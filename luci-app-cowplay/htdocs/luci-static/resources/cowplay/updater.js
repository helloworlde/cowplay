'use strict';
'require poll';
'require cowplay.rpc as cp';
'require cowplay.confirmation as confirmation';

return L.Class.extend({
    render: function(isCurrent, onBusy, canStart) {
        var self = this, state = {}, busy = false, checking = false, selectionChanged = false, statusKnown = false;
        var channel = E('select', {class: 'cbi-input-select', 'aria-label': '更新渠道', change: function() { selectionChanged = true; sync(); }}, [E('option', {value: 'stable'}, '正式版'), E('option', {value: 'dev'}, '开发版（Pre-release）')]);
        var tag = E('input', {type: 'text', class: 'cbi-input-text', placeholder: '留空检查所选渠道的最新版本', 'aria-label': 'GitHub 发布标签', maxlength: 64, input: function() { selectionChanged = true; sync(); }});
        var info = E('div'), message = E('p', {class: 'cw-foot', role: 'status', 'aria-live': 'polite'});
        var line = function(k, v) { return E('div', {class: 'cw-row'}, [E('span', {class: 'cw-key'}, k), E('span', {class: 'cw-value'}, v || '—')]); };
        var check = E('button', {type: 'button', class: 'btn cbi-button cbi-button-neutral cw-btn', click: async function() {
            if (!statusKnown || busy || checking || canStart && !canStart() || !L.hasViewPermission()) return;
            checking = true; sync();
            try { state = await cp.call('update_check', tag.value.trim(), channel.value); statusKnown = true; selectionChanged = false; draw(); }
            catch (e) { statusKnown = false; message.textContent = '检查失败：' + e.message; }
            finally { checking = false; sync(); }
        }}, '检查 GitHub 版本');
        var apply = E('button', {type: 'button', class: 'btn cbi-button cbi-button-negative cw-btn', click: async function() {
            if (!statusKnown || busy || selectionChanged || state.phase !== 'ready' || canStart && !canStart() || !L.hasViewPermission()) return;
            var id = state.id, version = state.version, selectedChannel = state.channel || 'stable';
            busy = true; sync();
            try {
                var current = function() { return statusKnown && !selectionChanged && channel.value === selectedChannel && (state.channel || 'stable') === selectedChannel && L.hasViewPermission() && state.id === id && state.version === version && state.phase === 'ready' && root.isConnected && (!isCurrent || isCurrent()); };
                var first = !(state.installed || {}).cowplay;
                if (!await confirmation.ask({key: 'cowplay-update', title: '安装 ' + version + '？', description: (selectedChannel === 'dev' ? '所选为开发预发布，功能与稳定性尚在验证。' : '所选为正式版。') + '将下载并验证核心与 LuCI 两个软件包，通过后停止 CowPlay 并在设备本地备份配置和私有状态。画面和声音会中断，配置与配对保留。' + (first ? '首次核心安装会开启下次开机自启，当前不启动。' : '完成后恢复原运行和自启状态。') + '安装失败时不会自动降级或还原状态，请保留 LuCI 管理入口。', confirm: '确认安装', current: current}) || !current()) return;
                state = await cp.call('update_apply', id); draw();
            } catch (e) { statusKnown = false; message.textContent = '安装请求未确认：' + e.message; }
            finally { busy = false; sync(); }
        }}, '安装所选版本');
        var acknowledge = E('button', {type: 'button', class: 'btn cbi-button cbi-button-neutral cw-btn', click: async function() {
            if (!statusKnown || busy || !L.hasViewPermission()) return;
            var id = state.id; busy = true; sync();
            try {
                var current = function() { return statusKnown && state.id === id && ['failed', 'interrupted'].indexOf(state.phase) >= 0 && L.hasViewPermission() && root.isConnected && (!isCurrent || isCurrent()); };
                if (!await confirmation.ask({key: 'cowplay-update-recovery', title: '确认已检查设备状态？', description: '请先在 LuCI 软件包和启动项中核对核心与 LuCI 的安装版本、服务和备份。本操作只解除更新任务锁定，保留备份，不修复软件包、不还原配置，也不启动服务。', confirm: '已检查，解除锁定', current: current}) || !current()) return;
                state = await cp.call('update_acknowledge', id); draw();
            } catch (e) { message.textContent = '解除锁定失败：' + e.message; }
            finally { busy = false; sync(); }
        }}, '检查恢复后解除锁定');
        function sync() {
            var active = ['checking', 'queued', 'downloading', 'stopping', 'backing_up', 'installing', 'starting'].indexOf(state.phase) >= 0;
            if (onBusy) onBusy(!statusKnown || active || !!state.locked);
            check.disabled = !statusKnown || !!(canStart && !canStart()) || busy || checking || active || !!state.locked || !!state.configuration_error || !L.hasViewPermission();
            tag.disabled = check.disabled;
            channel.disabled = check.disabled;
            apply.hidden = state.phase !== 'ready'; apply.disabled = !statusKnown || selectionChanged || !!(canStart && !canStart()) || busy || checking || state.phase !== 'ready' || !!state.configuration_error || !L.hasViewPermission();
            acknowledge.hidden = ['failed', 'interrupted'].indexOf(state.phase) < 0; acknowledge.disabled = !statusKnown || busy || checking || !L.hasViewPermission();
            check.textContent = checking || state.phase === 'checking' ? '正在检查…' : '检查 GitHub 版本';
        }
        function draw() {
            var installed = state.installed || {};
            if (!selectionChanged) channel.value = state.channel || 'stable';
            info.replaceChildren(line('GitHub 仓库', state.repository), line('已安装核心 / LuCI', (installed.cowplay || '未安装') + ' / ' + (installed['luci-app-cowplay'] || '未安装')), line('候选渠道', state.channel === 'dev' ? '开发版（Pre-release）' : '正式版'), line('所选版本 / 架构', state.version ? state.version + ' / ' + state.arch : '—'));
            if (state.backup) info.appendChild(line('设备本地备份', state.backup));
            message.textContent = state.configuration_error || state.installed_error || state.message || '选择正式版或开发版渠道，再检查最新版本；也可输入所选渠道的版本标签。'; sync();
        }
        async function refresh() {
            if (checking) return;
            try { state = await cp.call('update_status'); statusKnown = true; draw(); }
            catch (e) { statusKnown = false; message.textContent = '无法读取更新状态：' + e.message + '。LuCI 更新期间可能短暂重载，请稍后刷新。'; sync(); acknowledge.disabled = true; }
        }
        var root = E('div', {class: 'cw-card', 'data-cw-updater': ''}, [info, line('更新渠道', channel), line('版本标签', tag), E('div', {class: 'cw-actions'}, [check, apply, acknowledge]), message, E('p', {class: 'cw-foot'}, '正式版与开发版分开检查，安装前验证签名和兼容性。更新会短暂中断画面和声音，配置与配对保留。')]);
        draw(); check.disabled = true; if (onBusy) onBusy(true); refresh(); poll.add(refresh, 2);
        return root;
    }
});
