// Delivery-protocol statistics, split out of bundle.rs (see FIXES.md) so the
// queueing/ack/satellite-fallback logic in bundle.rs isn't interleaved with
// the histogram/counter bookkeeping that logic reports into.

use super::params::DvDtnParams;

/// Cumulative outcomes. Every bundle that leaves circulation lands in exactly
/// one of the `delivered` / `dropped_*` / `satellite` counters — the harness
/// asserts that, since a bundle that quietly vanishes would be invisible in the
/// UI and fatal to the delivery statistics.
#[derive(Debug, Default, Clone, Copy)]
pub struct BundleStats {
    pub originated: u64,
    pub delivered: u64,
    pub dropped_loop: u64,
    pub dropped_ttl: u64,
    /// Bundles that aged past BUNDLE_MAX_AGE_ROUNDS before reaching a tower and
    /// were handed to satellite instead of dropped — the release valve for a
    /// structurally saturated ground link (see MESH_COMMS_DESIGN.md §4).
    pub satellite: u64,
    /// Handoffs that failed because the receiver was already carrying. Not a
    /// loss — the sender keeps the bundle and retries on its next slot — so this
    /// is a congestion *pressure* gauge, not an outcome, and is excluded from
    /// `resolved()`.
    pub blocked: u64,

    // --- Acks ----------------------------------------------------------------
    //
    // Every bundle that reaches a tower spawns an ack that must independently
    // survive its own trip back. These are server truth about that trip;
    // `Balloon::outstanding` is the poorer view the origin itself can see.
    /// Acks that completed the full reverse path back to their origin.
    pub acked: u64,
    /// Acks that aged out, or were dropped by a full ack_queue, before
    /// completing the reverse path. The bundle they ack still counts as
    /// `delivered` — this is "arrived, but the receipt didn't survive."
    pub ack_lost: u64,

    // --- Slot accounting ----------------------------------------------------
    //
    // A bundle's entire budget is BUNDLE_MAX_AGE_ROUNDS / BEACON_INTERVAL_ROUNDS
    // = 30 wake slots. Every slot where the holder wakes holding a bundle and
    // fails to move it burns part of that budget, so the *stall rate* — not the
    // loss counters above — is what decides whether bundles arrive in time.
    // These break that down by cause.
    /// Wake slots where the holder had at least one bundle in hand.
    pub slots_with_bundle: u64,
    /// ...of which the holder had no route belief at all.
    pub stall_no_belief: u64,
    /// ...of which the believed next hop was no longer a neighbour.
    pub stall_stale_next_hop: u64,
    /// ...of which the balloon believed it could hear a tower, but couldn't.
    pub stall_tower_gone: u64,

    // --- Path length --------------------------------------------------------
    //
    // Indexed by hop count, saturating at the last bucket. Compare
    // `belief_hops` against true mesh depth (bin/mesh_depth.rs): if believed
    // distance runs well past topological distance, routing is sending bundles
    // the long way round and the expiry budget is being spent on detours.
    /// Hops actually travelled by bundles that reached a tower.
    pub delivered_hops: [u64; 32],
    /// Hops travelled before running out of time and going to satellite.
    pub satellite_hops: [u64; 32],
    /// Believed distance-to-tower, sampled once per wake slot per balloon.
    pub belief_hops: [u64; 32],

    // --- The last hop -------------------------------------------------------
    //
    // Every bundle in the world has to be handed to a tower by a balloon that
    // can currently hear one, and each such balloon can pass exactly one bundle
    // per duty-cycle slot. That makes the tower-adjacent population, not the
    // mesh at large, the throughput limit on delivery.
    /// Summed over rounds: balloons with a tower in radio range.
    pub tower_adjacent_samples: u64,
    /// Rounds over which the above was sampled.
    pub rounds_sampled: u64,
}

pub fn bump(hist: &mut [u64; 32], n: usize) {
    hist[n.min(31)] += 1;
}

impl BundleStats {
    /// Bundles that have left circulation, however they left. Satellite
    /// delivery is a resolution, not a loss — nothing ages out into the void
    /// anymore, it just changes channel.
    pub fn resolved(&self) -> u64 {
        self.delivered + self.dropped_loop + self.dropped_ttl + self.satellite
    }

    /// Wake slots that actually moved a bundle (forwarded, delivered, or
    /// dropped). `blocked` is subtracted too: a handoff refused by a full
    /// receiver wastes the slot exactly like a stale next hop does, even though
    /// it costs no bundle.
    pub fn slots_used(&self) -> u64 {
        self.slots_with_bundle.saturating_sub(
            self.stall_no_belief
                + self.stall_stale_next_hop
                + self.stall_tower_gone
                + self.blocked,
        )
    }

    /// Fraction of held-bundle wake slots wasted. This is the number that
    /// decides delivery: at a mean depth of d hops a bundle needs d successful
    /// slots out of the 30 it will ever get.
    pub fn stall_rate(&self) -> f64 {
        if self.slots_with_bundle == 0 {
            return 0.0;
        }
        1.0 - self.slots_used() as f64 / self.slots_with_bundle as f64
    }

    /// Mean number of balloons that could hand a bundle straight to a tower.
    pub fn mean_tower_adjacent(&self) -> f64 {
        if self.rounds_sampled == 0 {
            return 0.0;
        }
        self.tower_adjacent_samples as f64 / self.rounds_sampled as f64
    }

    /// Ceiling on deliveries per round, from the last hop alone: each
    /// tower-adjacent balloon can pass `tower_contact_bundles` per
    /// `beacon_interval_rounds`. Nothing about mesh depth, queue depth, or
    /// routing quality can raise it — only the tower-adjacent population or the
    /// size of a contact.
    pub fn delivery_capacity_per_round(&self, params: &DvDtnParams) -> f64 {
        self.mean_tower_adjacent() * params.tower_contact_bundles as f64
            / params.beacon_interval_rounds as f64
    }

    /// Delivery among bundles that actually finished. `delivered / originated`
    /// counts everything still legitimately in flight at the cutoff as a
    /// failure, which understates delivery badly at these origination rates.
    pub fn completion_rate(&self) -> f64 {
        if self.resolved() == 0 {
            return 0.0;
        }
        self.delivered as f64 / self.resolved() as f64
    }
}

/// Mean of a saturating histogram, ignoring the empty case.
pub fn hist_mean(h: &[u64; 32]) -> f64 {
    let n: u64 = h.iter().sum();
    if n == 0 {
        return 0.0;
    }
    h.iter().enumerate().map(|(i, &c)| i as f64 * c as f64).sum::<f64>() / n as f64
}
