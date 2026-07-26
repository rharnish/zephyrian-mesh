// Ported from cesium-app/src/towerModel.js (the plain data half of a tower —
// no Cesium entity/canvas rendering here, that stays client-side).

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Tower {
    pub id: u32,
    pub lon: f64,
    pub lat: f64,
    pub height_m: f64,
    /// Beacon wave counter — incremented on every transmission so balloons can
    /// tell a fresh wave from a stale one (see beacon.rs).
    #[serde(skip)]
    pub beacon_epoch: u64,
    /// Next comms round this tower transmits a beacon.
    #[serde(skip)]
    pub next_beacon_round: u64,
}

impl Tower {
    pub fn new(id: u32, lon: f64, lat: f64, height_m: f64) -> Self {
        Tower { id, lon, lat, height_m, beacon_epoch: 0, next_beacon_round: 0 }
    }
}
