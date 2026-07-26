// Read one snapshot straight from sim-server's WebSocket, bypassing the
// browser entirely.
//
// Why this exists: the headless browser runs on software GL and lags the
// server's 20 Hz snapshot rate, sometimes by many seconds. A `eval` read of
// server-driven state can therefore report the *old* value long after the
// server applied a change, which reads exactly like a broken feature. This
// script is ground truth — if the value is right here, the server is fine and
// any disagreement is browser lag.
//
// Usage:
//   node .claude/skills/run-cesium-app/peek.mjs
//   node .claude/skills/run-cesium-app/peek.mjs 's.meanDegree'
//   node .claude/skills/run-cesium-app/peek.mjs 's.balloons.filter(b => b.grounded).length'
//
// The optional argument is a JS expression evaluated with `s` bound to the
// snapshot object. Without it you get a summary of the scalar fields plus
// array lengths, which is usually enough to see what the server thinks.

const expr = process.argv[2];
const url = process.env.SIM_SERVER_WS ?? 'ws://127.0.0.1:8080/ws';

const ws = new WebSocket(url);

ws.onmessage = (event) => {
  let snapshot;
  try {
    snapshot = JSON.parse(event.data);
  } catch (e) {
    console.error('could not parse snapshot:', e.message);
    process.exit(1);
  }

  if (expr) {
    const value = new Function('s', `return (${expr});`)(snapshot);
    console.log(typeof value === 'object' ? JSON.stringify(value, null, 2) : value);
  } else {
    const summary = {};
    for (const [k, v] of Object.entries(snapshot)) {
      if (Array.isArray(v)) summary[`${k}.length`] = v.length;
      else if (v === null || typeof v !== 'object') summary[k] = v;
    }
    console.log(JSON.stringify(summary, null, 2));
  }
  ws.close();
  process.exit(0);
};

ws.onerror = () => {
  console.error(`could not connect to ${url} — is sim-server running? (ss -ltnp | grep :8080)`);
  process.exit(1);
};

setTimeout(() => {
  console.error('timed out waiting for a snapshot');
  process.exit(1);
}, 10000);
