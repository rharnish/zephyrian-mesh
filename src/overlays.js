// Overlay classification and the palette both the Cesium tints and the HTML
// panels draw from.
//
// Deliberately free of any Cesium import: everything here is pure functions
// over snapshot data plus CSS color strings, so it is the one part of the
// rendering layer that can be unit-tested in plain Node. Callers that need
// `Cesium.Color` build it from these strings once (see balloonLayer.js);
// callers building HTML use the strings directly. Before this module the same
// hex values were written out four times — here, in the mesh-health readout,
// in the inspector's verdict table, and again in the panel's legend markup —
// and could drift apart silently.

// Belief-vs-truth overlay (MESH_COMMS_DESIGN.md §3). The server sends each
// balloon's own belief (`believedHops`, learned only from beacons that reached
// it) alongside the union-find ground truth (`grounded`). The two disagreeing
// is the expected behavior of a duty-cycled mesh, not an error — this overlay
// is how you watch it happen.
export const BELIEF_CSS = {
  ok: '#5fd08a',      // believes, and is right
  stale: '#e05561',   // believes a route it has lost
  unaware: '#e0a355', // has a route, hasn't heard yet
  none: '#6a6f78',    // no belief, no route
};

// Legend text (control panel) and inspector verdict, keyed the same way so a
// new belief state can't be added to one and forgotten in the other.
export const BELIEF_LEGEND = {
  ok: 'believes, correct',
  stale: 'stale belief',
  unaware: 'unaware of route',
  none: 'no route known',
};
export const BELIEF_VERDICT = {
  ok: 'belief matches reality',
  stale: 'stale — that route is gone',
  unaware: 'a route exists, not heard yet',
  none: 'isolated, and knows it',
};

// Last-delivery overlay (MESH_COMMS_DESIGN.md §3). `lastChannel` is server
// truth about how a balloon's most recently *resolved* bundle actually got
// through — deliberately not something the balloon itself could report, since
// satellite delivery is silent to the origin (see bundle.rs).
export const DELIVERY_CSS = {
  radio: '#8de05f',     // lime — delivered over the mesh
  satellite: '#3fa7ff', // blue — release-valve delivery
  none: '#8a8f98',      // gray — nothing resolved yet
};
export const DELIVERY_LEGEND = {
  radio: 'radio mesh',
  satellite: 'satellite',
  none: 'nothing resolved yet',
};

// Used wherever a value is absent rather than meaningful — a pending ack, a
// channel that hasn't resolved, the placeholder hash columns.
export const MUTED_CSS = '#8a8f98';

// Bundle outcome colors. `acked`/`ackDied` intentionally reuse the belief
// palette: "delivered and acked" and "believes, and is right" are the same
// kind of good news, and showing them in different greens would imply a
// distinction that isn't there.
export const COMMS_OUTCOME_CSS = {
  acked: BELIEF_CSS.ok,
  ackDied: BELIEF_CSS.stale,
  satellite: DELIVERY_CSS.satellite,
  droppedInMesh: MUTED_CSS,
};

export function beliefKey(b) {
  const believes = b.believedHops !== null && b.believedHops !== undefined;
  if (believes) return b.grounded ? 'ok' : 'stale';
  return b.grounded ? 'unaware' : 'none';
}

export function deliveryKey(b) {
  return b.lastChannel ?? 'none';
}

// Share (0..100) of balloons in each delivery bucket, for the panel legend.
//
// Derived here rather than sent by the server: `lastChannel` is already on
// every balloon in the snapshot, so a server-computed aggregate would be
// restating data the client is holding — the same redundancy the per-edge
// coordinates were.
//
// The three buckets are exhaustive and mutually exclusive, so each is counted
// directly rather than deriving the last from the others. That means rounding
// can leave the displayed values summing to 99 or 101, which is preferable to
// a bucket that silently absorbs the error.
export function deliveryMix(balloons) {
  const counts = { radio: 0, satellite: 0, none: 0 };
  for (const b of balloons) {
    const key = deliveryKey(b);
    // An unrecognized channel would otherwise vanish from a legend that claims
    // to cover everything; count it as unresolved rather than dropping it.
    if (counts[key] === undefined) counts.none += 1;
    else counts[key] += 1;
  }
  const total = balloons.length;
  if (total === 0) return { radio: 0, satellite: 0, none: 0 };
  return {
    radio: (100 * counts.radio) / total,
    satellite: (100 * counts.satellite) / total,
    none: (100 * counts.none) / total,
  };
}

// The single classification of "how did this bundle end up", shared by the
// packet animation, the inspector summary, and (via commsAckLabel) the comms
// log. These three used to each carry their own copy of this branch, which is
// how the animation and the summary could disagree about the same bundle.
//
// Returns one of the COMMS_OUTCOME_CSS keys. `lastBundle` is assumed already
// resolved — callers check for a missing `path` (still Pending) first.
export function bundleOutcome(lb) {
  if (lb.channel === 'satellite') return 'satellite';
  if (lb.state === 'acked') return 'acked';
  // Reached a tower over the mesh, but the ack didn't make it all the way
  // home — `ackHopsCompleted` says how far it got.
  if (lb.channel === 'radio' && lb.ackHopsCompleted !== null && lb.ackHopsCompleted !== undefined) {
    return 'ackDied';
  }
  // Never reached a tower at all — dropped in the mesh (loop/TTL), so no ack
  // was ever spawned.
  return 'droppedInMesh';
}

// Per-record ack state for the comms log. Unlike `bundleOutcome` this works
// from a retained TelemetryRecord, which carries `ackState` directly and has
// a `pending` case the resolved-bundle view doesn't.
export function commsAckLabel(ackState, ackHopsCompleted, hops) {
  if (ackState === 'acked') return { text: 'acked', css: COMMS_OUTCOME_CSS.acked };
  if (ackState === 'pending') return { text: 'pending', css: MUTED_CSS };
  // timedOut
  if (ackHopsCompleted !== null && ackHopsCompleted !== undefined) {
    return { text: `died @ ${ackHopsCompleted}/${hops}`, css: COMMS_OUTCOME_CSS.ackDied };
  }
  return { text: 'timed out', css: MUTED_CSS };
}
