import * as Cesium from 'cesium';

// ---------------------------------------------------------------------------
// LinkLayer — owns a PolylineCollection of radio-link lines, reconciled
// against the edge list sim-server sends. Same shape as WindVectorField: the
// viewer is passed in per call rather than held.
//
// Link *detection* lives in sim-server (see link_detection.rs and its
// brute-force oracle test); this only draws what arrives.
// ---------------------------------------------------------------------------

const GROUNDED_LINK_COLOR = Cesium.Color.LIME.withAlpha(0.6);   // cluster reaches a tower
const UNGROUNDED_LINK_COLOR = Cesium.Color.GRAY.withAlpha(0.5); // balloon-only cluster

// Node keys are "b12" / "t3" — a one-character kind followed by an id. Pure,
// so the parsing is testable without a scene.
export function parseNodeKey(key) {
  return { kind: key[0], id: Number(key.slice(1)) };
}

export class LinkLayer {
  constructor() {
    this.collection = null;
    // pairKey -> { primitive, aKey, bKey }, so unchanged links are reused
    // instead of destroyed/recreated every tick.
    this.links = new Map();
  }

  _ensureCollection(viewer) {
    if (!this.collection) {
      this.collection = new Cesium.PolylineCollection();
      viewer.scene.primitives.add(this.collection);
    }
    return this.collection;
  }

  sync(viewer, edges) {
    const collection = this._ensureCollection(viewer);
    const edgesByPairKey = new Map(edges.map((e) => [e.pairKey, e]));

    // Remove links that no longer exist.
    for (const [pairKey, link] of this.links) {
      if (!edgesByPairKey.has(pairKey)) {
        collection.remove(link.primitive);
        this.links.delete(pairKey);
      }
    }
    // Add or update current links.
    for (const edge of edges) {
      const [aKey, bKey] = edge.pairKey.split('|');
      const posA = Cesium.Cartesian3.fromDegrees(edge.a[0], edge.a[1], edge.a[2]);
      const posB = Cesium.Cartesian3.fromDegrees(edge.b[0], edge.b[1], edge.b[2]);
      const color = edge.grounded ? GROUNDED_LINK_COLOR : UNGROUNDED_LINK_COLOR;
      const existing = this.links.get(edge.pairKey);
      if (existing) {
        existing.primitive.positions = [posA, posB];
        existing.primitive.material.uniforms.color = color;
      } else {
        const primitive = collection.add({
          positions: [posA, posB],
          width: 2,
          material: Cesium.Material.fromType('Color', { color }),
        });
        this.links.set(edge.pairKey, { primitive, aKey, bKey });
      }
    }
  }

  // Re-anchors every line to its endpoints' *current* positions, every tick —
  // not just on the throttled ticks where sim-server recomputes edge topology.
  // Otherwise a link's line stays frozen at its endpoints' positions as of the
  // last topology recompute while the balloon billboards keep moving every
  // tick, which reads as a "ghost edge" detached from its nodes until the next
  // recompute catches it up (most visible after something briefly stalls the
  // main thread, e.g. a 2D/3D scene-mode morph).
  //
  // `resolvePosition` maps a node key to a Cartesian3, or undefined if that
  // node isn't currently rendered.
  refreshPositions(resolvePosition) {
    for (const link of this.links.values()) {
      const posA = resolvePosition(link.aKey);
      const posB = resolvePosition(link.bKey);
      if (posA && posB) {
        link.primitive.positions = [posA, posB];
      }
    }
  }
}
