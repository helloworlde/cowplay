// Explicit decisions at the operation under test; never a global auto-accept.
import assert from 'node:assert/strict';
export async function luciConfirmation(b, decision = 'accept') {
    await b.wait(() => b.evaluate("document.body.classList.contains('modal-overlay-active') && !!document.querySelector('.cw-operation-confirmation')"), 'LuCI operation confirmation');
    assert.ok(await b.evaluate("document.querySelector('.cw-operation-confirmation p').textContent.length > 10"), 'confirmation describes the effect');
    await b.evaluate(`document.querySelector('[data-cw-confirm=${decision}]').click()`);
    await b.wait(() => b.evaluate("!document.body.classList.contains('modal-overlay-active') || !document.querySelector('.cw-operation-confirmation')"), 'LuCI confirmation closed');
}
