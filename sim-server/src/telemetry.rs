// What a balloon actually measured — the payload half of docs/design/MESH_COMMS_DESIGN.md
// §1's telemetry bundle. Until now a `Bundle` was pure routing metadata with
// the payload explicitly left out, because the environmental sensor block needs
// the ISA model that `atmosphere.rs` now provides.
//
// A record exists in two copies, and the distinction matters:
//
//   * the copy **retained** by the origin, in `Balloon::log` — bounded, and the
//     thing C3's hash chain will eventually sign;
//   * the copy that **travels**, inside the `Bundle` — consumed at a tower.
//
// Same event, two views. They are created together at origination and share a
// `seq` for that reason.
//
// **No cryptography here yet.** C3 proper adds `prev_hash`/`hash`/`sig` to this
// struct plus a key epoch; this module is where they go. A hash chain without
// signatures would not be tamper-evident — anyone editing a record could simply
// recompute every subsequent hash — so the chain belongs with the signing work
// rather than ahead of it.

use crate::atmosphere;
use crate::balloon::Balloon;
use serde::Serialize;

/// One telemetry sample from one balloon at one moment.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TelemetryRecord {
    pub origin_id: u32,
    /// Shared with the bundle carrying this record — they are the same event.
    pub seq: u64,
    pub created_at_round: u64,

    pub lon: f64,
    pub lat: f64,
    pub alt_m: f64,

    pub temperature_k: f64,
    pub pressure_hpa: f64,
    /// Synthesized rather than measured — see `atmosphere::humidity_pct`.
    pub humidity_pct: f64,

    // --- Outcome, filled in once this record's bundle resolves (see
    // `bundle::snapshot_resolved`) — `None`/`Pending` at creation. This is
    // what turns the retained log into the C4 comms-log panel (§3): a
    // per-record history, not just the single most recent outcome.
    pub channel: Option<crate::protocol::dv_dtn::bundle::Channel>,
    pub tower_id: Option<u32>,
    pub ack_state: crate::protocol::dv_dtn::bundle::AckState,
    pub hops: Option<u32>,
    pub ack_hops_completed: Option<u32>,
}

impl TelemetryRecord {
    /// Sample the atmosphere wherever this balloon currently is.
    ///
    /// §1 of the design also lists `gas`, `ballast` and `health` in the bundle.
    /// Those are P1 buoyancy state that does not exist yet, and are left out
    /// rather than filled with plausible-looking constants — a stubbed number
    /// that reads as real is worse than an absent one.
    /// `seq` is passed rather than read off the balloon: the sequence counter
    /// belongs to the comms protocol (see dv_dtn.rs), not to the balloon's
    /// physical state, even though the record it stamps is a measurement of
    /// that balloon.
    pub fn sample(balloon: &Balloon, seq: u64, round: u64) -> Self {
        TelemetryRecord {
            origin_id: balloon.id,
            seq,
            created_at_round: round,
            lon: balloon.lon,
            lat: balloon.lat,
            alt_m: balloon.alt,
            temperature_k: atmosphere::temperature_k(balloon.alt),
            pressure_hpa: atmosphere::pressure_hpa(balloon.alt),
            humidity_pct: atmosphere::humidity_pct(balloon.lon, balloon.lat, balloon.alt),
            channel: None,
            tower_id: None,
            ack_state: crate::protocol::dv_dtn::bundle::AckState::Pending,
            hops: None,
            ack_hops_completed: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn samples_the_atmosphere_at_the_balloons_own_position() {
        let b = Balloon::new(7, 12.0, -34.0, 18_000.0);
        let r = TelemetryRecord::sample(&b, 3, 42);

        assert_eq!(r.origin_id, 7);
        assert_eq!(r.seq, 3, "record seq must track the bundle seq it ships with");
        assert_eq!(r.created_at_round, 42);
        assert_eq!((r.lon, r.lat, r.alt_m), (12.0, -34.0, 18_000.0));

        // 18 km is in the isothermal layer.
        assert!((r.temperature_k - atmosphere::T11).abs() < 1e-9);
        assert!(r.pressure_hpa > 0.0 && r.pressure_hpa < 100.0, "got {}", r.pressure_hpa);
        assert!(r.humidity_pct >= 0.0 && r.humidity_pct < 1.0, "stratosphere should be dry");
    }

    /// Two balloons at the same altitude but different places must not report
    /// identical telemetry, or the field would look synthetic at a glance.
    #[test]
    fn records_differ_across_the_field() {
        let a = TelemetryRecord::sample(&Balloon::new(0, 0.0, 0.0, 8_000.0), 0, 0);
        let b = TelemetryRecord::sample(&Balloon::new(1, 80.0, 25.0, 8_000.0), 0, 0);
        assert!((a.humidity_pct - b.humidity_pct).abs() > 1e-6);
        // Temperature and pressure are altitude-only in the ISA, so those match.
        assert_eq!(a.temperature_k, b.temperature_k);
    }
}
