import * as Cesium from 'cesium';

// ---------------------------------------------------------------------------
// BeaconLayer — animates one tower's beacon wavefront: a dot per hop,
// traveling from the transmitting node to the balloon it reached, hop by hop
// as the wave relays outward (docs/design/MESH_COMMS_DESIGN.md §3 "Beacon
// wavefront"). Same traveling-dot language as CommsReplay's bundle replay,
// for visual consistency.
//
// Unlike CommsReplay (one dot reused sequentially along a known path), a
// wavefront is many concurrent, independently-timed hops — every awake
// balloon and every beaconing tower transmits on its own schedule — so this
// tracks a set of live per-hop animations rather than a single one.
//
// `snapshot.beaconHops` carries every tower's hops, unfiltered; this layer is
// handed the currently-watched tower id and discards the rest itself (see
// main.js) rather than the server doing that filtering.
// ---------------------------------------------------------------------------

const HOP_DURATION_MS = 1100;
const DOT_COLOR = Cesium.Color.fromCssColorString('#7fe0ff');

export class BeaconLayer {
  constructor() {
    this.active = new Map(); // hopKey -> { entity, frame }
    this.seen = new Set(); // hopKeys already animated in the current watch session
    this.token = 0;
  }

  // Starts a fresh watch session: clears whatever was playing and forgets
  // which hops have already been shown, so switching towers (or re-watching
  // the same one) doesn't skip hops or bleed animations from the old tower.
  reset(viewer) {
    this.token++;
    this._clearActive(viewer);
    this.seen.clear();
  }

  // Toggling watch off is the same as starting a (empty) fresh session.
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

  // `hops` is the full (unfiltered) snapshot.beaconHops array, or undefined.
  // `watchedTowerId` null means nothing should render. `resolveNodePosition`
  // takes a wire node key ("b12"/"t3") and returns its current Cartesian3, or
  // undefined if that node isn't currently visible/resolvable.
  handleSnapshot(viewer, hops, watchedTowerId, resolveNodePosition) {
    if (watchedTowerId === null || watchedTowerId === undefined || !hops) return;
    for (const hop of hops) {
      if (hop.towerId !== watchedTowerId) continue;
      const key = `${hop.from}|${hop.to}|${hop.epoch}|${hop.hopCount}`;
      if (this.seen.has(key)) continue;
      this.seen.add(key);
      const from = resolveNodePosition(hop.from);
      const to = resolveNodePosition(hop.to);
      if (!from || !to) continue; // an endpoint isn't currently drawn
      this._animateHop(viewer, key, from, to);
    }
  }

  _animateHop(viewer, key, from, to) {
    const token = this.token;
    const entity = viewer.entities.add({
      position: from,
      point: { pixelSize: 8, color: DOT_COLOR, outlineColor: Cesium.Color.BLACK, outlineWidth: 1 },
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
