/// Counters for spray-and-wait. Almost none of `BundleStats` transfers: there
/// are no beliefs to stall on, no next hops to go stale, no acks to lose, and
/// no hop budget to exhaust. What replaces them is the cost of replication —
/// how much of the traffic was redundant.
#[derive(Debug, Clone, Copy, Default)]
pub struct EpidemicStats {
    pub originated: u64,
    /// First arrival of a given record at a tower. Counts records, not copies,
    /// so it stays comparable with dv-dtn's `delivered`.
    pub delivered: u64,
    /// Later arrivals of a record already delivered. This is what replication
    /// buys robustness with, and it has no counterpart in a unicast protocol —
    /// the number that says whether the copy budget is set sensibly.
    pub duplicate_arrivals: u64,
    /// Records whose copies all aged out before any reached a tower.
    pub satellite: u64,
    /// Individual copies dropped on age. Several per record, by design.
    pub copies_expired: u64,
    /// Copy handoffs that actually happened.
    pub handoffs: u64,
    /// Handoffs refused because the receiver was full. Held, not lost.
    pub blocked: u64,
    /// Wake slots where a holder in the spray phase had no neighbour that
    /// lacked the record — the replication equivalent of a stall, and the
    /// thing that limits spread in a sparse field.
    pub no_candidate: u64,
}

impl EpidemicStats {
    pub fn resolved(&self) -> u64 {
        self.delivered + self.satellite
    }

    pub fn completion_rate(&self) -> f64 {
        if self.resolved() == 0 {
            return 0.0;
        }
        self.delivered as f64 / self.resolved() as f64
    }

    /// Copies transmitted per record actually delivered. The price of
    /// replication, and the number to put next to dv-dtn's delivery figures
    /// when asking whether the robustness was worth the airtime.
    pub fn handoffs_per_delivery(&self) -> f64 {
        if self.delivered == 0 {
            return 0.0;
        }
        self.handoffs as f64 / self.delivered as f64
    }
}
