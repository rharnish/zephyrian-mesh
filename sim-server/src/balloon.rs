// Ported from cesium-app/src/balloon.js. Wind-driven horizontal motion +
// simple buoyancy/ballast controller (target-altitude thermostat) for
// vertical motion.

use crate::config::{
    BALLOON_MAX_ALT, BALLOON_MIN_ALT, MAX_VERTICAL_RATE, TARGET_DRIFT_CHANCE_PER_SIM_HOUR,
    TARGET_DRIFT_RANGE, VERTICAL_GAIN,
};
use crate::wind_field::WindField;
use rand::Rng;
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Balloon {
    pub id: u32,
    pub lon: f64,
    pub lat: f64,
    pub alt: f64,
    #[serde(skip)]
    pub target_alt: f64,
    /// Hops to a tower as this balloon *believes*; `None` if it currently
    /// knows of no route. This is what the balloon would act on.
    ///
    /// Published here each tick from the comms protocol's own state (see
    /// dv_dtn.rs) rather than stored here: the balloon is physics, and which
    /// protocol is running decides whether a "believed hop count" even exists
    /// as a concept.
    pub believed_hops: Option<u32>,
    /// Ground truth from union-find — whether it can *actually* reach a tower.
    /// Sent alongside `believed_hops` purely so the UI can show where the two
    /// disagree; nothing in the simulation may read this on a balloon's behalf.
    pub grounded: bool,
    /// How this balloon's most recently *resolved* bundle actually got
    /// through — server truth (§3 of the design doc), not the balloon's own
    /// view: satellite delivery is silent to the origin, so this is
    /// deliberately something the balloon itself could never know. `None`
    /// until its first bundle resolves. Published from the protocol each tick,
    /// same as `believed_hops`.
    pub last_channel: Option<crate::bundle::Channel>,
}

impl Balloon {
    pub fn new(id: u32, lon: f64, lat: f64, alt: f64) -> Self {
        Balloon {
            id,
            lon,
            lat,
            alt,
            target_alt: alt,
            believed_hops: None,
            grounded: false,
            last_channel: None,
        }
    }

    pub fn step(&mut self, dt_seconds: f64, wind: &WindField, rng: &mut impl Rng) {
        let (u, v) = wind.sample(self.lon, self.lat, self.alt);

        // Horizontal: pure wind advection.
        let meters_per_deg_lat = 111_320.0;
        let meters_per_deg_lon = 111_320.0 * self.lat.to_radians().cos();
        self.lat += (v * dt_seconds) / meters_per_deg_lat;
        self.lon += (u * dt_seconds) / meters_per_deg_lon;
        // Wrap back into [-180, 180).
        self.lon = ((self.lon + 180.0) % 360.0 + 360.0) % 360.0 - 180.0;

        // Vertical: proportional target-altitude controller. Retargeting is a
        // rate per simulated hour, scaled by dt like the advection above, so it
        // doesn't silently change meaning when TIME_SCALE moves.
        if rng.gen::<f64>() < TARGET_DRIFT_CHANCE_PER_SIM_HOUR * dt_seconds / 3600.0 {
            let delta = (rng.gen::<f64>() * 2.0 - 1.0) * TARGET_DRIFT_RANGE;
            self.target_alt = (self.target_alt + delta).clamp(BALLOON_MIN_ALT, BALLOON_MAX_ALT);
        }
        let vertical_rate =
            (VERTICAL_GAIN * (self.target_alt - self.alt)).clamp(-MAX_VERTICAL_RATE, MAX_VERTICAL_RATE);
        self.alt += vertical_rate * dt_seconds;
        self.alt = self.alt.clamp(BALLOON_MIN_ALT, BALLOON_MAX_ALT);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng;
    use rand::rngs::StdRng;

    #[test]
    fn zero_wind_target_equals_current_stays_put() {
        let wind = WindField::zero();
        let mut b = Balloon::new(0, 10.0, 20.0, 18000.0);
        let mut rng = StdRng::seed_from_u64(42);
        let (lon0, lat0, alt0) = (b.lon, b.lat, b.alt);
        b.step(60.0, &wind, &mut rng);
        // Zero wind -> no horizontal motion. Target==current alt at spawn,
        // so vertical rate is 0 too (barring a random target-drift tick,
        // vanishingly unlikely with this seed over one step).
        assert_eq!(b.lon, lon0);
        assert_eq!(b.lat, lat0);
        assert!((b.alt - alt0).abs() < 1e-9);
    }

    #[test]
    fn nonzero_wind_moves_position() {
        let mut wind = WindField::zero();
        wind.levels[0].u_data = vec![vec![10.0]]; // eastward wind
        let mut b = Balloon::new(0, 10.0, 20.0, 18000.0);
        let mut rng = StdRng::seed_from_u64(42);
        b.step(60.0, &wind, &mut rng);
        assert!(b.lon > 10.0);
    }

    /// Retargeting must happen at the same rate per *simulated* hour no matter
    /// how much simulated time a tick covers.
    ///
    /// The bug this guards against: the chance was once expressed per tick, so
    /// changing TIME_SCALE silently rescaled it. Dropping TIME_SCALE from 60 to
    /// 15 would have quadrupled retargets per simulated hour — and since
    /// altitude churn is what breaks radio links, it would have surfaced only as
    /// an unexplained rise in the stale-belief fraction, far from its cause.
    #[test]
    fn retarget_rate_is_independent_of_tick_duration() {
        const SIM_HOURS: f64 = 5_000.0;
        let wind = WindField::zero();
        let expected = TARGET_DRIFT_CHANCE_PER_SIM_HOUR * SIM_HOURS;

        for (i, &dt) in [15.0_f64, 60.0, 240.0].iter().enumerate() {
            let steps = (SIM_HOURS * 3600.0 / dt) as usize;
            // Mid-range altitude so the ±4000 m jump is never clamped away.
            let mut b = Balloon::new(0, 0.0, 0.0, 13000.0);
            let mut rng = StdRng::seed_from_u64(7 + i as u64);
            let mut prev = b.target_alt;
            let mut retargets = 0u32;
            for _ in 0..steps {
                b.step(dt, &wind, &mut rng);
                if b.target_alt != prev {
                    retargets += 1;
                    prev = b.target_alt;
                }
            }
            let err = (retargets as f64 - expected).abs() / expected;
            assert!(
                err < 0.15,
                "dt={dt}s: {retargets} retargets over {SIM_HOURS} sim hours, \
                 expected ~{expected:.0} ({:.0}% off)",
                err * 100.0
            );
        }
    }

    #[test]
    fn longitude_wraps_at_antimeridian() {
        let mut wind = WindField::zero();
        wind.levels[0].u_data = vec![vec![1000.0]]; // strong eastward wind
        let mut b = Balloon::new(0, 179.9, 20.0, 18000.0);
        let mut rng = StdRng::seed_from_u64(1);
        b.step(600.0, &wind, &mut rng);
        assert!(b.lon >= -180.0 && b.lon < 180.0);
    }
}
