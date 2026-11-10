'use strict';
'require poll';
'require cowplay.rpc as cp';
'require cowplay.confirmation as confirmation';

return L.Class.extend({
    render: function(isCurrent, onBusy, canStart) {
        var self = this, state = {}, busy = false, checking = false;
        var tag = E('input', {type: 'text', class: 'cbi-input-text', placeholder: '留空使用最新稳定版', 'aria-label': 'GitHub 发布标签', maxlength: 64});
        var info = E('div'), message = E('p', {class: 'cw-foot', role: 'status', 'aria-live': 'polite'});
        var line = function(k, v) { return E('div', {class: 'cw-row'}, [E('span', {class: 'cw-key'}, k), E('span', {class: 'cw-value'}, v || '—')]); };
        var check = E('button', {type: 'button', class: 'btn cbi-button cbi-button-neutral cw-btn', click: async function() {
            if (busy || checking || canStart && !canStart() || !L.hasViewPermission()) return;
            checking = true; sync();
            try { state = await cp.call('update_check', tag.value.trim()); draw(); }
            catch (e) { message.textContent = '检查失败：' + e.message; }
            finally { checking = false; sync(); }
        }}, '检查 GitHub 版本');
        var apply = E('button', {type: 'button', class: 'btn cbi-button cbi-button-negative cw-btn', click: async function() {
            if (busy || state.phase !== 'ready' || canStart && !canStart() || !L.hasViewPermission()) return;
            var id = state.id, version = state.version;
            busy = true; sync();
            try {
                var current = function() { return L.hasViewPermission() && state.id === id && state.version === version && state.phase === 'ready' && root.isConnected && (!isCurrent || isCurrent()); };
                var first = !(state.installed || {}).cowplay;
                if (!await confirmation.ask({key: 'cowplay-update', title: '安装 ' + version + '？', description: '将下载并验证核心与 LuCI 两个软件包，通过后停止 CowPlay 并在设备本地备份配置和私有状态。画面和声音会中断，配置与配对保留。' + (first ? '首次核心安装会开启下次开机自启，当前不启动。' : '完成后恢复原运行和自启状态。') + '安装失败时不会自动降级或还原状态，请保留 SSH 管理入口。', confirm: '确认安装', current: current}) || !current()) return;
                state = await cp.call('update_apply', id); draw();
            } catch (e) { message.textContent = '安装未开始：' + e.message; }
            finally { busy = false; sync(); }
        }}, '安装所选版本');
        var prepare = E('button', {type: 'button', class: 'btn cbi-button cbi-button-neutral cw-btn', click: async function() {
            if (busy || state.phase !== 'current' || canStart && !canStart() || !L.hasViewPermission()) return;
            var id = state.id; busy = true; sync();
            try {
                var current = function() { return state.id === id && state.phase === 'current' && root.isConnected && (!isCurrent || isCurrent()) && L.hasViewPermission(); };
                if (!await confirmation.ask({key: 'cowplay-system-cache', title: '准备系统升级恢复缓存？', description: '将下载并验证当前版本的双软件包，在设备本地保存用于保留设置的 OpenWrt 系统升级恢复。当前服务不受影响。只有发布清单声明兼容且新固件依赖齐全时才自动恢复；sysupgrade -n 会清除缓存与配置。', confirm: '准备恢复缓存', current: current}) || !current()) return;
                state = await cp.call('update_prepare', id); draw();
            } catch(e) { message.textContent = '缓存准备失败：' + e.message; }
            finally { busy = false; sync(); }
        }}, '准备系统升级恢复缓存');
        var acknowledge = E('button', {type: 'button', class: 'btn cbi-button cbi-button-neutral cw-btn', click: async function() {
            if (busy || !L.hasViewPermission()) return;
            var id = state.id; busy = true; sync();
            try {
                var current = function() { return state.id === id && ['failed', 'interrupted'].indexOf(state.phase) >= 0 && L.hasViewPermission() && root.isConnected && (!isCurrent || isCurrent()); };
                if (!await confirmation.ask({key: 'cowplay-update-recovery', title: '确认已检查设备状态？', description: '请先通过 SSH 核对核心与 LuCI 的安装版本、服务和备份。本操作只解除更新任务锁定，保留备份，不修复软件包、不还原配置，也不启动服务。', confirm: '已检查，解除锁定', current: current}) || !current()) return;
                state = await cp.call('update_acknowledge', id); draw();
            } catch (e) { message.textContent = '解除锁定失败：' + e.message; }
            finally { busy = false; sync(); }
        }}, '检查恢复后解除锁定');
        function sync() {
            var active = ['checking', 'queued', 'downloading', 'stopping', 'backing_up', 'installing', 'starting'].indexOf(state.phase) >= 0;
            if (onBusy) onBusy(active || !!state.locked);
            check.disabled = !!(canStart && !canStart()) || busy || checking || active || !!state.locked || !!state.configuration_error || !L.hasViewPermission();
            tag.disabled = check.disabled;
            apply.hidden = state.phase !== 'ready'; apply.disabled = !!(canStart && !canStart()) || busy || checking || state.phase !== 'ready' || !!state.configuration_error || !L.hasViewPermission();
            prepare.hidden = state.phase !== 'current'; prepare.disabled = busy || checking || !!(canStart && !canStart()) || !!state.configuration_error || !L.hasViewPermission();
            acknowledge.hidden = ['failed', 'interrupted'].indexOf(state.phase) < 0; acknowledge.disabled = busy || checking || !L.hasViewPermission();
            check.textContent = checking || state.phase === 'checking' ? '正在检查…' : '检查 GitHub 版本';
        }
        function draw() {
            var installed = state.installed || {};
            info.replaceChildren(line('GitHub 仓库', state.repository), line('已安装核心 / LuCI', (installed.cowplay || '未安装') + ' / ' + (installed['luci-app-cowplay'] || '未安装')), line('所选版本 / 架构', state.version ? state.version + ' / ' + state.arch : '—'));
            var system = state.system_upgrade || {}; info.appendChild(line('系统升级恢复', system.blocked ? '应用与缓存状态需检查，系统恢复已锁定' : system.prepared ? '已缓存签名软件包；仅适用于声明兼容且保留设置的固件' : '尚未准备；手工安装后请检查当前版本并准备缓存'));
            if (system.result) info.appendChild(line('上次系统恢复', system.result));
            if (state.backup) info.appendChild(line('设备本地备份', state.backup));
            message.textContent = state.configuration_error || state.installed_error || state.message || '请检查最新稳定版，或输入 vX.Y.Z-rN 标签选择特定稳定版。'; sync();
        }
        async function refresh() {
            if (checking) return;
            try { state = await cp.call('update_status'); draw(); }
            catch (e) { if (onBusy) onBusy(true); message.textContent = '无法读取更新状态：' + e.message + '。LuCI 更新期间可能短暂重载，请稍后刷新。'; check.disabled = true; apply.disabled = true; acknowledge.disabled = true; prepare.disabled = true; }
        }
        var root = E('div', {class: 'cw-card', 'data-cw-updater': ''}, [info, line('版本标签', tag), E('div', {class: 'cw-actions'}, [check, apply, prepare, acknowledge]), message, E('p', {class: 'cw-foot'}, '只接受预置信任公钥签名的公开 GitHub 稳定版。架构和 OpenWrt 版本自动匹配；不支持降级。首次配置发布来源及公钥需要设备管理员通过 SSH 完成。')]);
        draw(); check.disabled = true; if (onBusy) onBusy(true); refresh(); poll.add(refresh, 2);
        return root;
    }
});
