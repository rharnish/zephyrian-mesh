// International Standard Atmosphere as a function of altitude — P1 of
// BALLOON_PHYSICS_VISION.md, and the model MESH_COMMS_DESIGN.md §1's telemetry
// sensor block reads from.
//
// The constants below are copied **verbatim** from
// `weather-data-server/wind_backend.py`, which is the source of truth named by
// the physics doc. That file converts pressure -> altitude to label the ERA5
// pressure levels the wind field is built from; this module is mostly the
// inverse (altitude -> pressure/temperature/density), so the two must agree or
// a balloon's reported pressure would disagree with the pressure level its wind
// was interpolated from. `altitude_m_from_pressure_hpa` is a direct port of the
// Python function kept for exactly that reason: the tests assert it is an exact
// inverse of `pressure_hpa`, which is what pins the Rust model to the Python one.
//
// Piecewise, matching the Python: troposphere to 11 km, then the isothermal
// lower stratosphere. Balloon altitudes of interest (15-25 km) sit mostly in the
// isothermal layer. Above ~20 km the real atmosphere's lapse rate changes again
// and this drifts, which is fine for a simulator and not fine for navigation.
//
// Scope: this is the ISA half of P1 only. Buoyancy, gas, ballast and the force
// integration in BALLOON_PHYSICS_VISION.md §1 are not here — `density_kg_m3` is
// provided because that work will need it, not because telemetry uses it.

use crate::config::{RH_SCALE_HEIGHT_M, RH_SPATIAL_AMPLITUDE, RH_SURFACE_PCT};

pub const P0: f64 = 1013.25; // hPa, sea-level standard pressure
pub const T0: f64 = 288.15; // K, sea-level standard temperature
pub const L: f64 = 0.0065; // K/m, tropospheric lapse rate
pub const P11: f64 = 226.32; // hPa, pressure at the 11 km tropopause
pub const T11: f64 = 216.65; // K, isothermal stratosphere temperature
pub const R: f64 = 8.31446; // J/(mol*K), universal gas constant
pub const G: f64 = 9.80665; // m/s^2
pub const M_AIR: f64 = 0.0289644; // kg/mol

/// Boundary between the two layers modelled here.
pub const TROPOPAUSE_M: f64 = 11_000.0;

/// Scale height of the isothermal layer, ~6341.7 m.
pub fn scale_height_strato() -> f64 {
    (R * T11) / (G * M_AIR)
}

/// Exponent in the barometric formula, ~5.25579. Note this is the *molar* form
/// (universal R with M_AIR), matching the Python — not the specific-gas-constant
/// form. Mixing the two silently changes the exponent.
fn barometric_exponent() -> f64 {
    (G * M_AIR) / (R * L)
}

/// Temperature at altitude. Linear lapse through the troposphere, then flat.
pub fn temperature_k(alt_m: f64) -> f64 {
    if alt_m <= TROPOPAUSE_M {
        T0 - L * alt_m
    } else {
        T11
    }
}

/// Pressure at altitude, in hectopascals.
pub fn pressure_hpa(alt_m: f64) -> f64 {
    if alt_m <= TROPOPAUSE_M {
        P0 * (temperature_k(alt_m) / T0).powf(barometric_exponent())
    } else {
        P11 * (-(alt_m - TROPOPAUSE_M) / scale_height_strato()).exp()
    }
}

/// Air density at altitude, from the ideal gas law. Unused by telemetry; the
/// buoyancy model in BALLOON_PHYSICS_VISION.md §1 is what needs it.
pub fn density_kg_m3(alt_m: f64) -> f64 {
    let p_pa = pressure_hpa(alt_m) * 100.0;
    p_pa * M_AIR / (R * temperature_k(alt_m))
}

/// Pressure -> altitude. A direct port of `pressure_hpa_to_altitude_m` in
/// `weather-data-server/wind_backend.py`, deliberately kept as the inverse of
/// `pressure_hpa` above: asserting the round trip is what proves the Rust
/// atmosphere and the Python one that labels the wind levels are the same model.
pub fn altitude_m_from_pressure_hpa(p_hpa: f64) -> f64 {
    if p_hpa <= 0.0 {
        return f64::NAN;
    }
    if p_hpa >= P11 {
        (T0 / L) * (1.0 - (p_hpa / P0).powf((R * L) / (G * M_AIR)))
    } else {
        TROPOPAUSE_M + scale_height_strato() * (P11 / p_hpa).ln()
    }
}

/// Relative humidity, in percent.
///
/// **Synthesized, not ISA.** The ISA has nothing to say about humidity, and the
/// ERA5 dataset behind the wind field is wind-only, so there is no measurement
/// to read — MESH_COMMS_DESIGN.md §1 calls for exactly this: "a
/// decreasing-with-altitude profile with noise". Swapping in real humidity is
/// the natural tie-in if the "extra variables" idea in WEATHER_BACKEND_PLAN.md
/// ever lands.
///
/// The "noise" is a smooth function of position rather than an RNG draw, on
/// purpose: `bundle::step` takes no RNG, and threading one in would make
/// `bin/protocol_sweep` runs irreproducible for the sake of a cosmetic field.
/// A low-frequency spatial field gives variation between balloons and stability
/// for one balloon across a run, which is what actually reads as plausible.
pub fn humidity_pct(lon: f64, lat: f64, alt_m: f64) -> f64 {
    let base = RH_SURFACE_PCT * (-alt_m / RH_SCALE_HEIGHT_M).exp();
    let perturb =
        1.0 + RH_SPATIAL_AMPLITUDE * (3.0 * lon.to_radians()).sin() * (2.0 * lat.to_radians()).cos();
    (base * perturb).clamp(0.0, 100.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64, tol: f64) -> bool {
        (a - b).abs() < tol
    }

    /// Textbook ISA values. If these drift, the model has been changed rather
    /// than refactored.
    #[test]
    fn matches_known_isa_anchors() {
        assert!(close(temperature_k(0.0), 288.15, 1e-9));
        assert!(close(pressure_hpa(0.0), 1013.25, 1e-6));
        assert!(close(density_kg_m3(0.0), 1.225, 1e-3), "got {}", density_kg_m3(0.0));

        assert!(close(temperature_k(TROPOPAUSE_M), 216.65, 1e-9));
        assert!(close(pressure_hpa(TROPOPAUSE_M), 226.32, 0.01));

        // Standard figure for 20 km is 54.7 hPa.
        assert!(close(pressure_hpa(20_000.0), 54.75, 0.05), "got {}", pressure_hpa(20_000.0));
    }

    /// The cross-validation that matters: `altitude_m_from_pressure_hpa` is a
    /// verbatim port of the function `wind_backend.py` uses to label pressure
    /// levels, so an exact round trip proves the Rust and Python atmospheres
    /// are the same model — not merely similar ones.
    #[test]
    fn is_an_exact_inverse_of_the_ported_python_conversion() {
        for &h in &[0.0, 1000.0, 5000.0, 10_000.0, 11_000.0, 15_000.0, 18_000.0, 20_000.0, 25_000.0]
        {
            let round_tripped = altitude_m_from_pressure_hpa(pressure_hpa(h));
            assert!(
                close(round_tripped, h, 0.01),
                "h={h} round-tripped to {round_tripped} via P={}",
                pressure_hpa(h)
            );
        }
    }

    #[test]
    fn pressure_decreases_monotonically_and_temperature_goes_flat() {
        let mut prev_p = f64::INFINITY;
        for step in 0..=50 {
            let h = step as f64 * 500.0; // 0 -> 25 km
            let p = pressure_hpa(h);
            assert!(p < prev_p, "pressure must strictly decrease, but {p} >= {prev_p} at {h} m");
            prev_p = p;

            if h < TROPOPAUSE_M {
                assert!(temperature_k(h) > T11, "troposphere should be warmer than the tropopause");
            } else {
                assert!(
                    close(temperature_k(h), T11, 1e-9),
                    "the modelled stratosphere is isothermal"
                );
            }
        }
    }

    #[test]
    fn humidity_is_bounded_and_dries_out_with_altitude() {
        let (lon, lat) = (12.0, 34.0);
        let mut prev = f64::INFINITY;
        for step in 0..=50 {
            let h = step as f64 * 500.0;
            let rh = humidity_pct(lon, lat, h);
            assert!((0.0..=100.0).contains(&rh), "RH out of range at {h} m: {rh}");
            assert!(rh < prev, "RH must fall with altitude, but {rh} >= {prev} at {h} m");
            prev = rh;
        }
        // The stratosphere is bone dry; anything else would look wrong on screen.
        assert!(humidity_pct(lon, lat, 18_000.0) < 1.0);
    }

    /// Humidity has to vary across the field, or synthesizing it bought nothing.
    ///
    /// Asserted over a grid rather than between two points on purpose: the
    /// perturbation is a product of a sine and a cosine, so it has nodal lines
    /// (every 60 deg of longitude, and at +/-45 deg latitude) where it is
    /// exactly zero. Two hand-picked coordinates can silently land on one and
    /// "prove" the field is flat when it isn't.
    #[test]
    fn humidity_varies_across_the_field_but_is_stable_for_one_place() {
        let mut samples = Vec::new();
        for lon_step in 0..12 {
            for lat_step in 0..6 {
                let lon = -180.0 + lon_step as f64 * 30.0;
                let lat = -75.0 + lat_step as f64 * 30.0;
                samples.push(humidity_pct(lon, lat, 2000.0));
            }
        }
        let min = samples.iter().cloned().fold(f64::INFINITY, f64::min);
        let max = samples.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        assert!(max - min > 1.0, "humidity should vary across the globe, got spread {}", max - min);

        // Same place, same answer — no RNG anywhere in here.
        assert_eq!(humidity_pct(23.0, 11.0, 2000.0), humidity_pct(23.0, 11.0, 2000.0));
    }
}
