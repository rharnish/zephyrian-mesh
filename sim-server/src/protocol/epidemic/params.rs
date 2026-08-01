/// Tunables for binary spray-and-wait.
///
/// Deliberately *not* a superset of `DvDtnParams`, and deliberately not shared
/// with it. Half of dv-dtn's constants describe things this protocol does not
/// have — belief expiry, beacon intervals, hop budgets — and folding both into
/// one struct would leave every protocol carrying fields it ignores, which is
/// exactly the shape the refactor was meant to get rid of.
///
/// What they do share is the duty cycle and the origination rate, because
/// those are properties of the radio and the payload rather than of the
/// routing, and keeping them comparable is what makes the two protocols
/// measurable against each other at all.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EpidemicParams {
    /// Copies a bundle starts with. The whole dial: 1 is direct-delivery-only
    /// (a balloon must itself meet a tower), while a large budget approaches
    /// plain epidemic flooding. Because binary spray halves the budget on each
    /// handoff, a bundle exists in at most this many instances network-wide,
    /// ever — which is what keeps replication bounded without a per-node
    /// summary vector of everything ever seen.
    pub copies: u32,
    /// Nominal gap between a node's transmissions, in comms rounds. Matches
    /// dv-dtn's beacon interval by default so the two protocols are compared
    /// under the same airtime budget rather than one being handed more.
    pub wake_interval_rounds: u64,
    /// +/- jitter on that gap, so the fleet doesn't transmit in lockstep.
    pub wake_jitter_rounds: u64,
    /// How often a balloon originates telemetry. Same default as dv-dtn, same
    /// reason.
    pub bundle_interval_rounds: u64,
    /// How long a copy lives before it is dropped. The origin's bundle counts
    /// as having gone to satellite once its copies are gone and nothing
    /// delivered it.
    pub bundle_max_age_rounds: u64,
    /// Copies a balloon may hold at once.
    pub relay_queue_capacity: usize,
    /// Copies handed over in a single tower contact.
    pub tower_contact: usize,
    /// Retained telemetry records per balloon.
    pub comms_log_capacity: usize,
}

impl Default for EpidemicParams {
    fn default() -> Self {
        EpidemicParams {
            copies: 8,
            wake_interval_rounds: 5,
            wake_jitter_rounds: 1,
            bundle_interval_rounds: 200,
            bundle_max_age_rounds: 150,
            relay_queue_capacity: 8,
            tower_contact: 4,
            comms_log_capacity: 32,
        }
    }
}
