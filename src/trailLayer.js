import * as Cesium from 'cesium';

// ---------------------------------------------------------------------------
// TrailLayer — draws a single balloon's flight path as it accumulates live.
// Same shape as LinkLayer: the viewer is passed in per call rather than held.
//
// Only one balloon is traced at a time. There's no history buffer — the
// trail starts empty the moment tracing begins and only ever grows forward
// from there, so it shows exactly what you've watched happen, nothing more.
// ---------------------------------------------------------------------------

const TRAIL_COLOR = Cesium.Color.YELLOW.withAlpha(0.85);
const TRAIL_WIDTH = 2;

// sim-server ticks at 20 Hz (TICK_INTERVAL_MS = 50) and each tick covers
// TICK_DT_SECONDS * TIME_SCALE = 1.0 * 15.0 = 15 simulated seconds, so the
// sim runs at 300x real time. Sampling every snapshot (the old behaviour)
// filled MAX_TRAIL_POINTS in a couple of real minutes — under a simulated
// day. Instead, sample on a fixed *simulated*-time cadence so a modest point
// budget spans several simulated days:
//
//   TRAIL_SAMPLE_EVERY_N_TICKS ticks = 20 * 15s = 300 sim-s = 5 sim-min/point
//   MAX_TRAIL_POINTS * 5 sim-min = 2000 * 5 = 10,000 sim-min ≈ 6.9 sim-days
//
// A full trail takes 2000 * (20 ticks / 20 ticks-per-sec) ≈ 33 real minutes
// to accumulate, but a multi-day glimpse is visible within a few real
// minutes of watching.
const TRAIL_SAMPLE_EVERY_N_TICKS = 100;
const MAX_TRAIL_POINTS = 10000;

export class TrailLayer {
  constructor() {
    this.collection = null;
    this.primitive = null;
    this.tracedId = null;
    this.points = []; // Cartesian3[], oldest first
    this.lastTick = null;
  }

  _ensureCollection(viewer) {
    if (!this.collection) {
      this.collection = new Cesium.PolylineCollection();
      viewer.scene.primitives.add(this.collection);
    }
    return this.collection;
  }

  isTracing(id) {
    return this.tracedId === id;
  }

  // Switches tracing to `id`, discarding any in-progress trail for whatever
  // was traced before.
  start(viewer, id) {
    this.stop(viewer);
    this.tracedId = id;
  }

  stop(viewer) {
    if (this.primitive && this.collection) {
      this.collection.remove(this.primitive);
    }
    this.primitive = null;
    this.tracedId = null;
    this.points = [];
    this.lastTick = null;
  }

  // Call once per snapshot with the server's tick counter and the traced
  // balloon's current position (or undefined if it isn't in the active set
  // right now). No-ops unless something is currently being traced, and only
  // appends a point once every TRAIL_SAMPLE_EVERY_N_TICKS — see the sampling
  // comment above for why.
  record(viewer, tick, position) {
    if (this.tracedId === null || !position) return;

    if (this.lastTick !== null) {
      if (tick < this.lastTick) {
        // Server restarted (or we reconnected to a fresh instance) — tick
        // count reset near 0. The in-progress trail no longer corresponds to
        // anything the new server knows about; start over rather than draw a
        // chord across half the planet.
        this.points = [];
        this.lastTick = null;
      } else if (tick - this.lastTick < TRAIL_SAMPLE_EVERY_N_TICKS) {
        return; // not time for another sample yet (this also makes pause a
        // no-op for free: the server re-sends the same tick while paused).
      }
    }
    this.lastTick = tick;

    this.points.push(position);
    if (this.points.length > MAX_TRAIL_POINTS) this.points.shift();
    if (this.points.length < 2) return;

    const collection = this._ensureCollection(viewer);
    if (this.primitive) {
      this.primitive.positions = this.points;
    } else {
      this.primitive = collection.add({
        positions: this.points,
        width: TRAIL_WIDTH,
        material: Cesium.Material.fromType('Color', { color: TRAIL_COLOR }),
      });
    }
  }
}
