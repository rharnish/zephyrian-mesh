// Ported from src/towerModel.js (the plain data half of a tower —
// no Cesium entity/canvas rendering here, that stays client-side).

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Tower {
    pub id: u32,
    pub lon: f64,
    pub lat: f64,
    pub height_m: f64,
}

impl Tower {
    pub fn new(id: u32, lon: f64, lat: f64, height_m: f64) -> Self {
        Tower { id, lon, lat, height_m }
    }
}
