// Ported from src/geo.js. Radio line-of-sight + great-circle
// distance, using each node's precomputed trig so repeated pairwise checks
// don't redo sin/cos/sqrt from scratch every time.

use crate::config::EARTH_RADIUS_M;
use rand::Rng;

pub fn to_radians(deg: f64) -> f64 {
    deg.to_radians()
}

pub fn horizon_km(height_m: f64, horizon_refraction_coeff: f64) -> f64 {
    horizon_refraction_coeff * height_m.max(0.0).sqrt()
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Precomputed {
    pub lat_rad: f64,
    pub lon_rad: f64,
    pub sin_lat: f64,
    pub cos_lat: f64,
    pub horizon_km: f64,
}

pub fn precompute(lon: f64, lat: f64, height_m: f64, horizon_refraction_coeff: f64) -> Precomputed {
    let lat_rad = to_radians(lat);
    let lon_rad = to_radians(lon);
    Precomputed {
        lat_rad,
        lon_rad,
        sin_lat: lat_rad.sin(),
        cos_lat: lat_rad.cos(),
        horizon_km: horizon_km(height_m, horizon_refraction_coeff),
    }
}

pub fn great_circle_distance_km_precomputed(a: &Precomputed, b: &Precomputed) -> f64 {
    let r = EARTH_RADIUS_M / 1000.0;
    let d_phi = b.lat_rad - a.lat_rad;
    let d_lambda = b.lon_rad - a.lon_rad;
    let sin_d_phi = (d_phi / 2.0).sin();
    let sin_d_lambda = (d_lambda / 2.0).sin();
    let h = sin_d_phi * sin_d_phi + a.cos_lat * b.cos_lat * sin_d_lambda * sin_d_lambda;
    2.0 * r * h.sqrt().asin()
}

pub fn in_radio_range_precomputed(a: &Precomputed, b: &Precomputed) -> bool {
    let max_range = a.horizon_km + b.horizon_km;
    great_circle_distance_km_precomputed(a, b) <= max_range
}

/// Uniform random point on a sphere (avoids clustering near the poles that a
/// naive uniform-lat sample would produce).
pub fn random_global_position(rng: &mut impl Rng) -> (f64, f64) {
    let lon = rng.gen::<f64>() * 360.0 - 180.0;
    let lat = (rng.gen::<f64>() * 2.0 - 1.0).asin().to_degrees();
    (lon, lat)
}
