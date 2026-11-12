// Every call is an explicit decision in a synthetic user flow, never a global auto-accept.
import assert from 'node:assert/strict';
export async function confirmation(b,decision='confirm'){
 await b.wait(()=>b.evaluate("!!document.querySelector('.action-confirmation[open]')"),'explicit operation confirmation');
 assert.ok(await b.evaluate("document.querySelector('#action-confirmation-description').textContent.length>10"),'confirmation explains the effect');
 await b.evaluate(`document.querySelector('.action-confirmation .confirmation-${decision==='confirm'?'accept':'cancel'}').click()`);
 await b.wait(()=>b.evaluate("!document.querySelector('.action-confirmation[open]')"),'confirmation closed');
 await b.evaluate('new Promise(r=>requestAnimationFrame(()=>requestAnimationFrame(r)))');
}
