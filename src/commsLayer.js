import * as Cesium from 'cesium';

// ---------------------------------------------------------------------------
// CommsLayer — animates the protocol's actual transmissions: a dot per hop,
// travelling from the transmitting node to the one it reached, as they happen
// (docs/design/MESH_COMMS_DESIGN.md §3 "Beacon wavefront").
//
// Deliberately not beacon-specific. The server sends `snapshot.commsEvents`
// with a `kind` on each, and this styles by kind from a table, so a protocol
// whose discovery is a request/reply flood — or one with no discovery at all
// that only ever moves payload — animates through this same path with no
// change here. Unknown kinds fall back to a default style rather than
// vanishing, which is what makes adding a protocol cheap.
//
// Unlike CommsReplay (one dot reused along a single known path), a wavefront
// is many concurrent, independently-timed hops — every awake node transmits on
// its own schedule — so this tracks a set of live per-hop animations.
//
// `commsEvents` carries every tower's traffic, unfiltered; this layer is told
// which tower is being watched and discards the rest itself (see main.js),
// rather than the server keeping per-client selection state.
// ---------------------------------------------------------------------------

const HOP_DURATION_MS = 1100;

// Per-kind styling. `size` is the base pixel size, before payload weighting.
const DEFAULT_STYLE = { color: '#c8d2e0', size: 7 };
const KIND_STYLES = {
  routeAd: { color: '#7fe0ff', size: 8 },
  routeRequest: { color: '#ffd166', size: 8 },
  routeReply: { color: '#5fd08a', size: 8 },
  bundle: { color: '#e87ba4', size: 9 },
  ack: { color: '#5fd08a', size: 7 },
  gossip: { color: '#b39ddb', size: 7 },
};

// A transmission carrying several records draws as one heavier dot rather than
// N identical ones — otherwise batching and non-batching look identical on
// screen, which is exactly the comparison worth being able to see. Sub-linear,
// so a large batch stays legible rather than swamping the globe.
function sizeFor(style, payload) {
  const n = Math.max(1, payload || 1);
  return style.size + 4 * Math.log2(n);
}

export class CommsLayer {
  constructor() {
    this.active = new Map(); // eventKey -> { entity, frame }
    this.seen = new Set(); // keys already animated this watch session
    this.token = 0;
  }

  // Starts a fresh watch session: clears whatever was playing and forgets what
  // has been shown, so switching towers (or re-watching the same one) doesn't
  // skip hops or bleed animations from the old tower.
  reset(viewer) {
    this.token++;
    this._clearActive(viewer);
    this.seen.clear();
  }

  // Toggling watch off is the same as starting a fresh (empty) session.
  stop(viewer) {
    this.reset(viewer);
  }

  _clearActive(viewer) {
    for (const { entity, frame } of this.active.values()) {
      if (frame !== null) cancelAnimationFrame(frame);
      viewer.entities.remove(entity);
    }
    this.active.clear();
  }

  // `events` is the full (unfiltered) snapshot.commsEvents array, or undefined.
  // `watchedTowerId` null means nothing renders. `resolveNodePosition` takes a
  // wire node key ("b12"/"t3") and returns its current Cartesian3, or
  // undefined if that node isn't currently drawn.
  handleSnapshot(viewer, events, watchedTowerId, resolveNodePosition) {
    if (watchedTowerId === null || watchedTowerId === undefined || !events) return;
    for (const e of events) {
      // An event with no tower of its own — a protocol that doesn't route from
      // towers — can't be attributed to the watched one, so it is skipped
      // rather than drawn for every selection.
      if (e.towerId !== watchedTowerId) continue;
      const key = `${e.kind}|${e.from}|${e.to}|${e.epoch}|${e.hopCount}`;
      if (this.seen.has(key)) continue;
      this.seen.add(key);
      const from = resolveNodePosition(e.from);
      const to = resolveNodePosition(e.to);
      if (!from || !to) continue; // an endpoint isn't currently drawn
      this._animate(viewer, key, from, to, e);
    }
  }

  _animate(viewer, key, from, to, event) {
    const token = this.token;
    const style = KIND_STYLES[event.kind] ?? DEFAULT_STYLE;
    const entity = viewer.entities.add({
      position: from,
      point: {
        pixelSize: sizeFor(style, event.payload),
        color: Cesium.Color.fromCssColorString(style.color),
        outlineColor: Cesium.Color.BLACK,
        outlineWidth: 1,
      },
    });
    const record = { entity, frame: null };
    this.active.set(key, record);

    const start = performance.now();
    const step = (now) => {
      if (token !== this.token) return; // superseded by reset()/stop()
      const t = Math.min(1, (now - start) / HOP_DURATION_MS);
      entity.position = Cesium.Cartesian3.lerp(from, to, t, new Cesium.Cartesian3());
      if (t >= 1) {
        viewer.entities.remove(entity);
        this.active.delete(key);
        return;
      }
      record.frame = requestAnimationFrame(step);
    };
    record.frame = requestAnimationFrame(step);
  }
}
