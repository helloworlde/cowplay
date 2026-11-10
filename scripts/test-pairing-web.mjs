// New /pair and /phone flows, plus the shared car settings surfaces.
// Token cookies replace the retired /pairing bearer-token form.
await import('./test-design-web.mjs');
const {runPairingContract} = await import('./lib/pairing-contract.mjs');
await runPairingContract('phone');
