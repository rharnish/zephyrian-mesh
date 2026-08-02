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
    /// Records that reached a terminal state. **This is a complete account,
    /// despite having two terms where `BundleStats::resolved` has four**: a
    /// record whose copies all expire is resolved to `Satellite` rather than
    /// dropped (see `record_outcome` in mod.rs), and there is no hop budget or
    /// next hop here to fail against, so `dropped_loop` and `dropped_ttl` have
    /// no counterpart rather than being unaccounted for.
    ///
    /// The trap is one line up, not here. **`copies_expired` is not comparable
    /// with `dropped_ttl`** — it counts individual *copies*, several per
    /// record by design, where every counter in `BundleStats` counts records.
    /// At `copies = 16` it runs to five figures beside dv-dtn's single digits,
    /// and reading that as a loss rate would be reading a different unit.
    pub fn resolved(&self) -> u64 {
        self.delivered + self.satellite
    }

    /// Same definition as `BundleStats::completion_rate`, and the same warning
    /// applies: it drops records still circulating at the cutoff, so it
    /// flatters a protocol that leaves them there. Pair it with
    /// `unresolved_share` and `delivered_per_originated`.
    pub fn completion_rate(&self) -> f64 {
        if self.resolved() == 0 {
            return 0.0;
        }
        self.delivered as f64 / self.resolved() as f64
    }

    /// Records originated that never reached a terminal state — still being
    /// carried, in some number of copies, when the run ended.
    pub fn unresolved(&self) -> u64 {
        self.originated.saturating_sub(self.resolved())
    }

    pub fn unresolved_share(&self) -> f64 {
        if self.originated == 0 {
            return 0.0;
        }
        self.unresolved() as f64 / self.originated as f64
    }

    pub fn delivered_per_originated(&self) -> f64 {
        if self.originated == 0 {
            return 0.0;
        }
        self.delivered as f64 / self.originated as f64
    }

    /// How much of the finished traffic the release valve carried. Under
    /// replication this is the headline failure mode — copies that wandered
    /// without finding a tower — so it is the number to read beside
    /// `no_candidate`.
    pub fn satellite_share(&self) -> f64 {
        if self.resolved() == 0 {
            return 0.0;
        }
        self.satellite as f64 / self.resolved() as f64
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
