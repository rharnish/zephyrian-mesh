import * as Cesium from 'cesium';

// Color ramp by wind speed (m/s): blue (calm) -> green -> yellow -> red (fast).
const SPEED_COLOR_STOPS = [
  { speed: 0, color: [80, 140, 220] },
  { speed: 15, color: [80, 200, 90] },
  { speed: 30, color: [230, 200, 40] },
  { speed: 50, color: [220, 60, 40] },
];

function speedToColor(speedMs) {
  for (let i = 0; i < SPEED_COLOR_STOPS.length - 1; i++) {
    const a = SPEED_COLOR_STOPS[i];
    const b = SPEED_COLOR_STOPS[i + 1];
    if (speedMs <= b.speed) {
      const t = (speedMs - a.speed) / (b.speed - a.speed);
      return a.color.map((c, idx) => Math.round(c + (b.color[idx] - c) * t));
    }
  }
  return SPEED_COLOR_STOPS[SPEED_COLOR_STOPS.length - 1].color;
}

// Visual scale: how long an arrow is per m/s of wind speed, and a cap so
// jet-stream-level speeds don't produce arrows the size of a continent.
const LENGTH_METERS_PER_MS = 4000;
const MAX_ARROW_LENGTH_M = 400000;
const ARROWHEAD_FRACTION = 0.3; // arrowhead segments as a fraction of shaft length
const ARROWHEAD_ANGLE_DEG = 25;
const MIN_SPEED_TO_DRAW = 0.5; // skip near-calm points, mostly clutter at that point

// Computes true 3D arrow geometry in the local east-north-up frame at
// (lon, lat, altitude), so direction reads correctly from any camera angle
// (not just top-down) — unlike screen-space-rotated billboards.
function computeArrowGeometry(lon, lat, altitudeM, u, v) {
  const speed = Math.sqrt(u * u + v * v);
  const lengthM = Math.min(speed * LENGTH_METERS_PER_MS, MAX_ARROW_LENGTH_M);
  const base = Cesium.Cartesian3.fromDegrees(lon, lat, altitudeM);
  const enu = Cesium.Transforms.eastNorthUpToFixedFrame(base);

  const dirEast = speed > 0 ? u / speed : 0;
  const dirNorth = speed > 0 ? v / speed : 0;
  const tip = Cesium.Matrix4.multiplyByPoint(
    enu,
    new Cesium.Cartesian3(dirEast * lengthM, dirNorth * lengthM, 0),
    new Cesium.Cartesian3()
  );

  const headLen = lengthM * ARROWHEAD_FRACTION;
  const angle = Math.atan2(dirNorth, dirEast);
  const headAngle = Cesium.Math.toRadians(ARROWHEAD_ANGLE_DEG);

  function headPoint(offset) {
    const a = angle + Math.PI + offset; // pointing back from the tip
    const local = new Cesium.Cartesian3(Math.cos(a) * headLen, Math.sin(a) * headLen, 0);
    const worldOffset = Cesium.Matrix4.multiplyByPointAsVector(enu, local, new Cesium.Cartesian3());
    return Cesium.Cartesian3.add(tip, worldOffset, new Cesium.Cartesian3());
  }

  return { base, tip, head1: headPoint(headAngle), head2: headPoint(-headAngle), speed };
}

// ---------------------------------------------------------------------------
// WindVectorField — owns a PolylineCollection of arrows for one wind level
// at a time. Call render() again (same or different level) to replace what's
// shown; clear() to remove it entirely.
// ---------------------------------------------------------------------------
export class WindVectorField {
  constructor() {
    this.collection = null;
  }

  clear(viewer) {
    if (this.collection) {
      viewer.scene.primitives.remove(this.collection);
      this.collection = null;
    }
  }

  // `stride` samples every Nth grid point in both lon and lat — bigger
  // stride means fewer, sparser arrows (cheaper, less cluttered). Default
  // bumped up since the underlying ERA5 grid is global and dense; stride=4
  // still leaves tens of thousands of sample points.
  render(viewer, windField, levelIndex, stride = 12) {
    this.clear(viewer);
    const level = windField.levels[levelIndex];
    if (!level) return;

    const { nx, ny, lo1, la1, dx, dy } = windField.header;
    const collection = new Cesium.PolylineCollection();

    for (let row = 0; row < ny; row += stride) {
      for (let col = 0; col < nx; col += stride) {
        const lon = lo1 + col * dx;
        const lat = la1 - row * dy;
        const u = level.u_data[row][col];
        const v = level.v_data[row][col];
        const speed = Math.sqrt(u * u + v * v);
        if (speed < MIN_SPEED_TO_DRAW) continue;

        const { base, tip, head1, head2 } = computeArrowGeometry(lon, lat, level.altitudeM, u, v);
        // Opaque (alpha 1.0): Cesium's PolylineCollection renders translucent
        // material with depthMask:false (no early-z), forcing full alpha-blend
        // compositing on every overlapping fragment across the whole visible
        // arrow field — a steady per-frame GPU cost, not a one-time build cost.
        // At thousands of arrows this was the FPS killer, not draw-call count.
        const [r, g, b] = speedToColor(speed);
        const color = new Cesium.Color(r / 255, g / 255, b / 255, 1.0);

        // Single continuous polyline tracing shaft + both arrowhead strokes
        // (base -> tip -> head1 -> back to tip -> head2). Same visual shape
        // as three separate polylines, but one primitive instead of three —
        // matters a lot at thousands of sample points.
        collection.add({
          positions: [base, tip, head1, tip, head2],
          width: 2,
          material: Cesium.Material.fromType('Color', { color }),
        });
      }
    }

    viewer.scene.primitives.add(collection);
    this.collection = collection;
  }
}
