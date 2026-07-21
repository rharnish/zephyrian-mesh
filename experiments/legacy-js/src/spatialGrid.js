// ---------------------------------------------------------------------------
// Spatial grid for neighbor queries — buckets nodes by (lon, lat) cell so
// radio-range checks only compare each node against nearby candidates
// instead of every other node (O(n) neighbors instead of O(n^2) pairs).
//
// IMPORTANT: a degree of longitude covers less physical distance the
// further you get from the equator (shrinks by cos(latitude)), while a
// degree of latitude is uniform everywhere (~111.32 km). A fixed 3x3 cell
// window is only "wide enough" at the equator — at higher latitudes the
// longitude window covers less real distance than the search actually
// needs, and valid in-range pairs can fall outside it entirely (a real
// missed-edge bug, not just an approximation). neighbors() below takes the
// caller's required range and widens the longitude window to compensate.
// ---------------------------------------------------------------------------
const KM_PER_DEG_LAT = 111.32;
const MIN_COS_LAT = 0.02; // clamp near the poles so the longitude window doesn't blow up to infinity

// Longitude wraps at +-180 (the antimeridian) but a bare floor(lon/cellSize)
// doesn't know that: a node at 179.9 deg and one at -179.9 deg are ~12 km
// apart in reality but land in cells ~60 columns apart, so a naive window
// search around either one never finds the other. Normalizing into a
// canonical [-180, 180) range before bucketing means the seam is the only
// place this matters, and neighbors() (below) re-normalizes each candidate
// longitude so the search window itself wraps around that seam too.
function normalizeLon(lon) {
  return ((lon + 180) % 360 + 360) % 360 - 180;
}

export class SpatialGrid {
  constructor(cellSizeDeg) {
    this.cellSizeDeg = cellSizeDeg;
    this.cells = new Map();
  }

  key(lon, lat) {
    const cx = Math.floor(normalizeLon(lon) / this.cellSizeDeg);
    const cy = Math.floor(lat / this.cellSizeDeg);
    return `${cx}:${cy}`;
  }

  clear() {
    this.cells.clear();
  }

  insert(item, lon, lat) {
    const k = this.key(lon, lat);
    if (!this.cells.has(k)) this.cells.set(k, []);
    this.cells.get(k).push(item);
  }

  // Returns candidates within `maxRangeKm` of (lon, lat) — actually within
  // a square cell-window guaranteed to CONTAIN that range, so callers still
  // need their own precise distance check afterward (this just avoids
  // missing candidates, it doesn't replace the exact check).
  neighbors(lon, lat, maxRangeKm) {
    const normLon = normalizeLon(lon);
    const cy = Math.floor(lat / this.cellSizeDeg);

    const cellKm = this.cellSizeDeg * KM_PER_DEG_LAT;
    const latCellSpan = Math.max(1, Math.ceil(maxRangeKm / cellKm));

    // Near either pole, a cosine-scaled longitude window can miss real
    // neighbors on the far side of the pole: two points can be close in
    // great-circle distance while differing in longitude by up to 180
    // degrees (going "over the top" instead of straight across). The cosine
    // scaling only accounts for longitude shrinking at *this* latitude — it
    // has no notion of wrapping over the pole into completely different
    // longitudes. So if the search radius reaches as far as the pole itself
    // (measured along a meridian), widen the longitude window to the full
    // circle instead of the cosine-scaled estimate.
    const distToNorthPoleKm = (90 - lat) * KM_PER_DEG_LAT;
    const distToSouthPoleKm = (lat + 90) * KM_PER_DEG_LAT;
    const nearAPole = distToNorthPoleKm <= maxRangeKm || distToSouthPoleKm <= maxRangeKm;

    const lonCellSpan = nearAPole
      ? Math.max(1, Math.ceil(180 / this.cellSizeDeg)) // full 360-degree sweep either direction
      : Math.max(1, Math.ceil(maxRangeKm / (cellKm * Math.max(Math.cos((lat * Math.PI) / 180), MIN_COS_LAT))));

    const results = [];
    for (let dx = -lonCellSpan; dx <= lonCellSpan; dx++) {
      // Step in real degrees and re-normalize (rather than cx + dx on the
      // raw integer cell index) so a window that overshoots +-180 wraps
      // around to the columns on the other side of the antimeridian.
      const cx = Math.floor(normalizeLon(normLon + dx * this.cellSizeDeg) / this.cellSizeDeg);
      for (let dy = -latCellSpan; dy <= latCellSpan; dy++) {
        const k = `${cx}:${cy + dy}`;
        if (this.cells.has(k)) results.push(...this.cells.get(k));
      }
    }
    return results;
  }
}
