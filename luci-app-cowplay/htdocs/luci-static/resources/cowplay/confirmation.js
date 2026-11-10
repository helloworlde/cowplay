'use strict';
'require ui';

var active;
return L.Class.extend({
    cancel: function(key) { if (active && (key == null || active.key === key)) active.finish(false); },
    ask: function(options) {
        if (active || document.body.classList.contains('modal-overlay-active')) return Promise.resolve(false);
        return new Promise(function(resolve) {
            var previous = document.activeElement, dialog, watch, finished = false;
            var body = E('div', {class: 'cw-operation-confirmation'}, [E('p', {}, options.description)]);
            var finish = function(approved) {
                if (finished) return; finished = true;
                clearInterval(watch); document.removeEventListener('keydown', keydown, true);
                var ownsModal = dialog.contains(body);
                if (ownsModal) ui.hideModal();
                active = null;
                if (ownsModal && previous && previous.isConnected) previous.focus();
                resolve(approved);
            };
            var keydown = function(ev) { if (ev.key === 'Escape') { ev.preventDefault(); ev.stopImmediatePropagation(); finish(false); } };
            var cancel = E('button', {type: 'button', 'data-cw-confirm': 'cancel', class: 'btn', click: function() { finish(false); }}, '取消');
            body.append(E('div', {class: 'cw-actions right'}, [cancel, E('button', {type: 'button', 'data-cw-confirm': 'accept', class: 'btn cbi-button-negative', click: function() { finish(!options.current || options.current()); }}, options.confirm || '确认') ]));
            dialog = ui.showModal(options.title, [body]); active = {key: options.key, finish: finish};
            document.addEventListener('keydown', keydown, true); cancel.focus();
            watch = setInterval(function() {
                if (!document.body.classList.contains('modal-overlay-active') || !dialog.contains(body) || options.current && !options.current()) finish(false);
            }, 100);
        });
    }
});
