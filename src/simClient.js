// Every conversation with sim-server, in one place: the snapshot stream in,
// and the user's commands out.
//
// No Cesium and no DOM — this module knows about the wire protocol and
// nothing about how any of it is drawn. Each command was previously written
// out at its call site as the same six-line fetch/headers/stringify/catch
// block, once per action, with the error message as the only difference.

import { SIM_SERVER_URL, SIM_SERVER_WS_URL } from './config.js';

const RECONNECT_DELAY_MS = 2000;

// Commands are fire-and-forget: the server applies them and echoes the result
// in the next snapshot, which is how a second browser tab picks up a change
// made in this one. Nothing here waits for or returns a result, so a failure
// is reported and dropped rather than propagated — `what` names the action so
// the console says which one gave up.
function postCommand(path, body, what) {
  return fetch(`${SIM_SERVER_URL}${path}`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(body),
  }).catch((e) => console.error(`Failed to ${what} on sim-server:`, e));
}

export function setPaused(paused) {
  return postCommand('/api/paused', { paused }, 'sync pause state');
}

export function setHorizonCoeff(coeff) {
  return postCommand('/api/horizon-coeff', { coeff }, 'sync horizon coefficient');
}

export function setBalloonCount(n) {
  return postCommand('/api/balloons/count', { n }, 'set balloon count');
}

export function addTower(lon, lat, heightM) {
  return postCommand('/api/towers', { lon, lat, heightM }, 'add tower');
}

export function removeTower(id) {
  return fetch(`${SIM_SERVER_URL}/api/towers/${id}`, { method: 'DELETE' }).catch((e) =>
    console.error('Failed to remove tower:', e)
  );
}

// The one *query* — everything above is fire-and-forget. Resolves to null on
// any failure so callers have a single "nothing to show" case rather than
// separate empty and error paths.
export async function fetchBalloonComms(id) {
  try {
    const res = await fetch(`${SIM_SERVER_URL}/api/balloons/${id}/comms`);
    if (!res.ok) return null;
    return await res.json();
  } catch (e) {
    console.error('Failed to fetch balloon comms:', e);
    return null;
  }
}

// Opens the snapshot stream and keeps it open, re-dialing on close. One
// authoritative stream; the caller only renders what arrives.
//
// Snapshots are *coalesced*: only the newest one is ever handed on, and at
// most one per animation frame. A snapshot is a complete picture of the world,
// so an older one that hasn't been drawn yet has no value once a newer one has
// landed — rendering it is work spent putting a state on screen that is
// already wrong.
//
// Parsing is deferred to drain time for the same reason — JSON.parse on a
// snapshot about to be discarded is pure cost.
//
// Be clear about what this does and does not fix. It only skips snapshots
// that have already been read off the socket and are queued in JS; measured
// on a loaded client that was a real saving (a few hundred renders skipped
// over half a minute). It does not fix the deeper lag, because most of the
// backlog is not here — it is unread bytes in the socket, which arrive in
// order and cannot be skipped. A client that cannot drain 20 Hz of snapshots
// falls behind no matter what this function does; fixing that needs the
// server to stop pushing faster than the client consumes, or to send deltas
// instead of whole worlds.
export function connectSimServer(onSnapshot) {
  const connect = () => {
    const ws = new WebSocket(SIM_SERVER_WS_URL);
    let pending = null; // newest raw message not yet drawn
    let scheduled = false;

    const drain = () => {
      scheduled = false;
      const raw = pending;
      pending = null;
      if (raw === null) return;
      onSnapshot(JSON.parse(raw));
    };

    ws.onmessage = (event) => {
      pending = event.data; // deliberately overwrites: newest wins
      if (!scheduled) {
        scheduled = true;
        // rAF rather than a timer: it paces to what the renderer can actually
        // draw, and stops entirely in a backgrounded tab instead of building
        // up work nobody is looking at.
        requestAnimationFrame(drain);
      }
    };
    ws.onerror = (e) => console.error('sim-server WebSocket error (is sim-server running?):', e);
    ws.onclose = () => {
      console.warn(`sim-server WebSocket closed — retrying in ${RECONNECT_DELAY_MS / 1000}s`);
      setTimeout(connect, RECONNECT_DELAY_MS);
    };
  };
  connect();
}
