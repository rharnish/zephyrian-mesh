// Ported from cesium-app/src/windField.js. Deserializes directly from the
// JSON shape wind_backend.py's /api/wind-levels already returns, so no
// changes are needed on the Python side.

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct WindHeader {
    pub nx: usize,
    pub ny: usize,
    pub lo1: f64,
    pub la1: f64,
    pub lo2: f64,
    pub la2: f64,
    pub dx: f64,
    pub dy: f64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Level {
    #[serde(rename = "pressureHpa")]
    pub pressure_hpa: f64,
    #[serde(rename = "altitudeM")]
    pub altitude_m: f64,
    pub u_data: Vec<Vec<f64>>,
    pub v_data: Vec<Vec<f64>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct WindField {
    pub header: WindHeader,
    pub levels: Vec<Level>,
}

impl WindField {
    /// Zero-wind fallback, mirroring main.js's fallback WindField when the
    /// backend fetch fails.
    pub fn zero() -> Self {
        WindField {
            header: WindHeader {
                nx: 1,
                ny: 1,
                lo1: -180.0,
                la1: 90.0,
                lo2: 180.0,
                la2: -90.0,
                dx: 360.0,
                dy: 180.0,
            },
            levels: vec![Level {
                pressure_hpa: 500.0,
                altitude_m: 18000.0,
                u_data: vec![vec![0.0]],
                v_data: vec![vec![0.0]],
            }],
        }
    }

    // Normalize a [-180, 180] longitude into the grid's own convention
    // (ERA5 grids are commonly 0-360).
    fn normalize_lon(&self, lon: f64) -> f64 {
        let (lo1, lo2) = (self.header.lo1, self.header.lo2);
        if lo1 >= 0.0 && lo2 > 180.0 && lon < 0.0 {
            lon + 360.0
        } else {
            lon
        }
    }

    // Bilinear interpolation of a single level's u/v grid at (lon, lat).
    fn sample_level(&self, level: &Level, lon: f64, lat: f64) -> (f64, f64) {
        let h = &self.header;
        let g_lon = self.normalize_lon(lon);

        let col_f = (g_lon - h.lo1) / h.dx;
        let row_f = (h.la1 - lat) / h.dy; // la1 is max lat (top row), dy positive downward

        let col0 = (col_f.floor() as i64).clamp(0, if h.nx >= 2 { h.nx as i64 - 2 } else { 0 }) as usize;
        let row0 = (row_f.floor() as i64).clamp(0, if h.ny >= 2 { h.ny as i64 - 2 } else { 0 }) as usize;
        let col1 = (col0 + 1).min(h.nx - 1);
        let row1 = (row0 + 1).min(h.ny - 1);

        let tx = (col_f - col0 as f64).clamp(0.0, 1.0);
        let ty = (row_f - row0 as f64).clamp(0.0, 1.0);

        let u00 = level.u_data[row0][col0];
        let u10 = level.u_data[row0][col1];
        let u01 = level.u_data[row1][col0];
        let u11 = level.u_data[row1][col1];
        let v00 = level.v_data[row0][col0];
        let v10 = level.v_data[row0][col1];
        let v01 = level.v_data[row1][col0];
        let v11 = level.v_data[row1][col1];

        let u = (1.0 - tx) * (1.0 - ty) * u00
            + tx * (1.0 - ty) * u10
            + (1.0 - tx) * ty * u01
            + tx * ty * u11;
        let v = (1.0 - tx) * (1.0 - ty) * v00
            + tx * (1.0 - ty) * v10
            + (1.0 - tx) * ty * v01
            + tx * ty * v11;
        (u, v)
    }

    /// Wind vector at (lon, lat, alt_m), blended between the two pressure
    /// levels that bracket alt_m. Levels must be sorted ascending by
    /// altitude_m (wind_backend.py already sorts them this way).
    pub fn sample(&self, lon: f64, lat: f64, alt_m: f64) -> (f64, f64) {
        let levels = &self.levels;
        if levels.is_empty() {
            return (0.0, 0.0);
        }
        if alt_m <= levels[0].altitude_m {
            return self.sample_level(&levels[0], lon, lat);
        }
        let last = levels.len() - 1;
        if alt_m >= levels[last].altitude_m {
            return self.sample_level(&levels[last], lon, lat);
        }

        let mut lower = &levels[0];
        let mut upper = &levels[last];
        for i in 0..last {
            if levels[i].altitude_m <= alt_m && levels[i + 1].altitude_m >= alt_m {
                lower = &levels[i];
                upper = &levels[i + 1];
                break;
            }
        }

        let (u_lower, v_lower) = self.sample_level(lower, lon, lat);
        let (u_upper, v_upper) = self.sample_level(upper, lon, lat);
        let span = upper.altitude_m - lower.altitude_m;
        let t = if span > 0.0 { (alt_m - lower.altitude_m) / span } else { 0.0 };

        (u_lower + (u_upper - u_lower) * t, v_lower + (v_upper - v_lower) * t)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tiny_field() -> WindField {
        // 2x2 grid, lon 0..10, lat 0..10 (la1=10 is top row).
        WindField {
            header: WindHeader { nx: 2, ny: 2, lo1: 0.0, la1: 10.0, lo2: 10.0, la2: 0.0, dx: 10.0, dy: 10.0 },
            levels: vec![Level {
                pressure_hpa: 500.0,
                altitude_m: 5000.0,
                u_data: vec![vec![0.0, 10.0], vec![20.0, 30.0]],
                v_data: vec![vec![0.0, 0.0], vec![0.0, 0.0]],
            }],
        }
    }

    #[test]
    fn samples_corners_exactly() {
        let f = tiny_field();
        assert_eq!(f.sample(0.0, 10.0, 5000.0), (0.0, 0.0)); // row0,col0
        assert_eq!(f.sample(10.0, 10.0, 5000.0), (10.0, 0.0)); // row0,col1
        assert_eq!(f.sample(0.0, 0.0, 5000.0), (20.0, 0.0)); // row1,col0
        assert_eq!(f.sample(10.0, 0.0, 5000.0), (30.0, 0.0)); // row1,col1
    }

    #[test]
    fn samples_center_as_average() {
        let f = tiny_field();
        let (u, _v) = f.sample(5.0, 5.0, 5000.0);
        assert!((u - 15.0).abs() < 1e-9);
    }

    #[test]
    fn blends_between_levels() {
        let mut f = tiny_field();
        f.levels.push(Level {
            pressure_hpa: 300.0,
            altitude_m: 9000.0,
            u_data: vec![vec![100.0, 100.0], vec![100.0, 100.0]],
            v_data: vec![vec![0.0, 0.0], vec![0.0, 0.0]],
        });
        let (u, _v) = f.sample(0.0, 10.0, 7000.0); // halfway between 5000 and 9000
        assert!((u - 50.0).abs() < 1e-9);
    }
}
