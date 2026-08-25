// Ported from experiments/legacy-js/src/spatialGrid.js. Buckets node indices by
// (lon, lat) cell so radio-range checks only compare each node against
// nearby candidates instead of every other node.
//
// See spatialGrid.js's header comment for why the longitude window has to
// widen with latitude (degrees of longitude shrink toward the poles) and why
// longitude bucketing has to re-normalize around the +-180 antimeridian seam.

use std::collections::HashMap;

const KM_PER_DEG_LAT: f64 = 111.32;
const MIN_COS_LAT: f64 = 0.02;

fn normalize_lon(lon: f64) -> f64 {
    ((lon + 180.0) % 360.0 + 360.0) % 360.0 - 180.0
}

pub struct SpatialGrid {
    cell_size_deg: f64,
    cells: HashMap<(i64, i64), Vec<usize>>,
}

impl SpatialGrid {
    pub fn new(cell_size_deg: f64) -> Self {
        SpatialGrid { cell_size_deg, cells: HashMap::new() }
    }

    fn key(&self, lon: f64, lat: f64) -> (i64, i64) {
        let cx = (normalize_lon(lon) / self.cell_size_deg).floor() as i64;
        let cy = (lat / self.cell_size_deg).floor() as i64;
        (cx, cy)
    }

    pub fn clear(&mut self) {
        self.cells.clear();
    }

    /// Like `clear()`, but keeps the HashMap's entries and each bucket's Vec
    /// capacity around instead of dropping them — cells tend to hold roughly
    /// the same nodes tick to tick, so this avoids repeated dealloc/realloc
    /// of the same buckets. Produces identical bucket contents to
    /// clear()+reinsert, just without the allocator churn.
    pub fn soft_clear(&mut self) {
        for bucket in self.cells.values_mut() {
            bucket.clear();
        }
    }

    pub fn insert(&mut self, index: usize, lon: f64, lat: f64) {
        let k = self.key(lon, lat);
        self.cells.entry(k).or_default().push(index);
    }

    /// Returns candidate indices within a square cell-window guaranteed to
    /// contain `max_range_km` of (lon, lat) — callers still need their own
    /// precise distance check afterward.
    pub fn neighbors(&self, lon: f64, lat: f64, max_range_km: f64) -> Vec<usize> {
        let norm_lon = normalize_lon(lon);
        let cy = (lat / self.cell_size_deg).floor() as i64;

        let cell_km = self.cell_size_deg * KM_PER_DEG_LAT;
        let lat_cell_span = ((max_range_km / cell_km).ceil() as i64).max(1);

        // Near either pole, a cosine-scaled longitude window can miss real
        // neighbors on the far side of the pole: two points can be close in
        // great-circle distance while differing in longitude by up to 180
        // degrees (going "over the top" instead of straight across). The
        // cosine scaling only accounts for longitude shrinking at *this*
        // latitude — it has no notion of wrapping over the pole into
        // completely different longitudes. So if the search radius reaches
        // as far as the pole itself (measured along a meridian), widen the
        // longitude window to the full circle instead.
        let dist_to_north_pole_km = (90.0 - lat) * KM_PER_DEG_LAT;
        let dist_to_south_pole_km = (lat + 90.0) * KM_PER_DEG_LAT;
        let near_a_pole = dist_to_north_pole_km <= max_range_km || dist_to_south_pole_km <= max_range_km;

        let lon_cell_span = if near_a_pole {
            // Wide enough to cover a full 360-degree sweep either direction.
            ((180.0 / self.cell_size_deg).ceil() as i64).max(1)
        } else {
            let cos_lat = lat.to_radians().cos().max(MIN_COS_LAT);
            ((max_range_km / (cell_km * cos_lat)).ceil() as i64).max(1)
        };

        let mut results = Vec::new();
        for dx in -lon_cell_span..=lon_cell_span {
            let cx = (normalize_lon(norm_lon + dx as f64 * self.cell_size_deg) / self.cell_size_deg).floor() as i64;
            for dy in -lat_cell_span..=lat_cell_span {
                if let Some(bucket) = self.cells.get(&(cx, cy + dy)) {
                    results.extend_from_slice(bucket);
                }
            }
        }
        results
    }
}
