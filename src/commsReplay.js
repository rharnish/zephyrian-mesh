import * as Cesium from 'cesium';

import { bundleOutcome, COMMS_OUTCOME_CSS } from './overlays.js';
import { toCesiumColors } from './cesiumColor.js';

// ---------------------------------------------------------------------------
// CommsReplay — animates one balloon's most recently *resolved* bundle
// (MESH_COMMS_DESIGN.md §3/C4).
//
// A dot travels the bundle's actual recorded path — not a recomputed shortest
// path — and then the ack's fate plays out: all the way back if acked, partway
// if it died en route, or not at all if the bundle went out via satellite or
// never reached a tower.
//
// This is a replay of the last resolved bundle, not a live view of one
// currently in flight: a bundle can take many rounds per hop, so watching one
// "live" would mostly look idle.
// ---------------------------------------------------------------------------

const PATH_COLOR = Cesium.Color.fromCssColorString('#ffd166');
const HOP_DURATION_MS = 550;
const OUTCOME_COLORS = toCesiumColors(COMMS_OUTCOME_CSS);

export class CommsReplay {
  constructor() {
    this.collection = null;
    this.pathPrimitive = null;
    this.packetEntity = null;
    this.frame = null;
    // Bumped on every clear so an in-flight animation's frame callback can
    // tell it's been superseded (new selection, or the same one re-fetched)
    // and stop touching an entity that may already be gone.
    this.token = 0;
  }

  _ensureCollection(viewer) {
    if (!this.collection) {
      this.collection = new Cesium.PolylineCollection();
      viewer.scene.primitives.add(this.collection);
    }
    return this.collection;
  }

  clear(viewer) {
    this.token++;
    if (this.frame !== null) {
      cancelAnimationFrame(this.frame);
      this.frame = null;
    }
    if (this.pathPrimitive) {
      this.collection.remove(this.pathPrimitive);
      this.pathPrimitive = null;
    }
    if (this.packetEntity) {
      viewer.entities.remove(this.packetEntity);
      this.packetEntity = null;
    }
  }

  // Animates a dot across a sequence of positions, one hop per
  // HOP_DURATION_MS, then calls `onDone`. Reuses the packet entity across legs
  // (outbound, then the ack's reverse leg) so the dot doesn't jump between
  // them.
  _animate(viewer, positions, color, onDone) {
    const token = this.token;
    if (!this.packetEntity) {
      this.packetEntity = viewer.entities.add({
        position: positions[0],
        point: { pixelSize: 10, color, outlineColor: Cesium.Color.BLACK, outlineWidth: 1 },
      });
    } else {
      this.packetEntity.point.color = color;
      this.packetEntity.position = positions[0];
    }
    if (positions.length < 2) {
      onDone();
      return;
    }
    let hop = 0;
    const totalHops = positions.length - 1;
    let hopStart = performance.now();
    const step = (now) => {
      if (token !== this.token) return; // superseded — stop touching this entity
      const t = Math.min(1, (now - hopStart) / HOP_DURATION_MS);
      this.packetEntity.position = Cesium.Cartesian3.lerp(
        positions[hop],
        positions[hop + 1],
        t,
        new Cesium.Cartesian3()
      );
      if (t >= 1) {
        hop++;
        if (hop >= totalHops) {
          onDone();
          return;
        }
        hopStart = now;
      }
      this.frame = requestAnimationFrame(step);
    };
    this.frame = requestAnimationFrame(step);
  }

  // Renders and animates the selected balloon's last resolved bundle.
  //
  // Positions are snapshotted once at render time — a deliberate replay of a
  // *past* path using each balloon's *current* position, the same "possibly
  // stale" idiom the rest of this design leans on rather than a hard error.
  //
  // `positionOfBalloon` and `positionOfTower` resolve hop ids to where those
  // nodes are drawn right now; either may return undefined.
  render(viewer, comms, positionOfBalloon, positionOfTower) {
    const lb = comms && comms.lastBundle;
    if (!lb || !lb.path) return; // still Pending, or nothing originated yet

    const positions = lb.path.map(positionOfBalloon).filter(Boolean);
    if (positions.length !== lb.path.length) return; // a hop balloon isn't currently visible

    // The recorded path only ever holds balloon ids — delivery to a tower is
    // modeled as instantaneous from the last tower-adjacent balloon, so the
    // tower itself is never a hop. Append its position so the drawn/animated
    // path actually reaches the tower instead of stopping one hop short.
    let towerIncluded = false;
    if (lb.towerId !== null && lb.towerId !== undefined) {
      const towerPos = positionOfTower(lb.towerId);
      if (towerPos) {
        positions.push(towerPos);
        towerIncluded = true;
      }
    }
    // Ack hops are counted purely over balloon-to-balloon hops (see
    // `Ack.total_hops` in bundle.rs) — the tower hand-off above was never a
    // modeled ack hop. The reverse path always starts at the tower though
    // (that's where the ack is created), so the partial-reverse slice always
    // shows that leg plus however many balloon hops the ack actually made.
    const balloonHops = lb.path.length - 1;
    const towerLeg = towerIncluded ? 1 : 0;

    this.pathPrimitive = this._ensureCollection(viewer).add({
      positions,
      width: 3,
      material: Cesium.Material.fromType('PolylineDash', { color: PATH_COLOR, dashLength: 12 }),
    });

    const outcome = bundleOutcome(lb);
    const outcomeColor = OUTCOME_COLORS[outcome];

    this._animate(viewer, positions, PATH_COLOR, () => {
      // Satellite pickup and a mesh drop both end the story where the outbound
      // leg stopped — there is no ack to animate, only a recolor.
      if (outcome === 'satellite' || outcome === 'droppedInMesh') {
        this.packetEntity.point.color = outcomeColor;
        return;
      }
      // The ack retraces the path. A completed one runs the whole way home;
      // one that died is truncated to however far it actually got.
      const reverse = [...positions].reverse();
      const ackPath =
        outcome === 'acked'
          ? reverse
          : reverse.slice(0, Math.max(0, Math.min(balloonHops, lb.ackHopsCompleted)) + towerLeg + 1);
      this._animate(viewer, ackPath, outcomeColor, () => {
        this.packetEntity.point.color = outcomeColor;
      });
    });
  }
}
