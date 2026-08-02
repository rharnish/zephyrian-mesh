// Delivery-protocol statistics, split out of bundle.rs (see FIXES.md) so the
// queueing/ack/satellite-fallback logic in bundle.rs isn't interleaved with
// the histogram/counter bookkeeping that logic reports into.

use super::params::DvDtnParams;
use crate::protocol::stats::LatencyStats;

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

    // --- Latency (design doc: the axis that was missing) ---------------------
    //
    // Everything above answers "did it arrive?". These answer "how long did that
    // take?" — the other half of the delay-tolerant trade, and the half that was
    // unmeasured until now. Three separate clocks, because they answer three
    // different questions and a mechanism can move them in opposite directions:
    /// Origination to tower. What the trip cost.
    pub delivery_latency: LatencyStats,
    /// Origination to the *origin finding out* — the full round trip, bundle
    /// out and receipt back. Always at least `delivery_latency`, and the gap
    /// between them is what the ack mechanism costs. This is the one a balloon
    /// actually experiences: until it fires, the balloon believes nothing has
    /// happened.
    pub ack_latency: LatencyStats,
    /// Origination to leaving the origin's queue at all. Isolates *discovery*
    /// wait from *forwarding* wait: a bundle that sat because its balloon had
    /// no route shows up here, and one that sat in a congested mesh does not.
    /// The reactive variants were expected to differ from proactive mainly in
    /// this term.
    pub first_hop_latency: LatencyStats,
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
    /// tower-adjacent balloon can pass `batch.tower_contact` per
    /// `beacon_interval_rounds`. Nothing about mesh depth, queue depth, or
    /// routing quality can raise it — only the tower-adjacent population or the
    /// size of a contact.
    pub fn delivery_capacity_per_round(&self, params: &DvDtnParams) -> f64 {
        self.mean_tower_adjacent() * params.batch.tower_contact as f64
            / params.beacon_interval_rounds as f64
    }

    /// Delivery among bundles that actually finished. `delivered / originated`
    /// counts everything still legitimately in flight at the cutoff as a
    /// failure, which understates delivery badly at these origination rates.
    ///
    /// **The bias runs both ways, and neither ratio is the honest one alone.**
    /// This one drops still-in-flight bundles from its denominator, so it
    /// *flatters* a protocol that strands them — and stranding is exactly the
    /// failure mode of reactive discovery, which spends whole wake slots
    /// holding bundles with nowhere to send them. Measured: reply overhearing
    /// reads −0.29 ± 0.42 here and **+3.91 ± 0.39** on `delivered_per_originated`,
    /// an opposite sign from the same runs. Report both, or say which bias you
    /// are accepting; `unresolved()` below is the quantity that separates them.
    pub fn completion_rate(&self) -> f64 {
        if self.resolved() == 0 {
            return 0.0;
        }
        self.delivered as f64 / self.resolved() as f64
    }

    /// Bundles originated that never reached *any* terminal state — still being
    /// carried when the run ended. Precisely what `completion_rate` discards,
    /// published so that discarding is visible rather than silent.
    pub fn unresolved(&self) -> u64 {
        self.originated.saturating_sub(self.resolved())
    }

    pub fn unresolved_share(&self) -> f64 {
        ratio(self.unresolved(), self.originated)
    }

    /// The other delivery ratio: of everything that was *started*, how much
    /// arrived. Pessimistic by exactly `unresolved_share`.
    pub fn delivered_per_originated(&self) -> f64 {
        ratio(self.delivered, self.originated)
    }

    /// How much of the finished traffic the release valve carried rather than
    /// the mesh. High values mean the mesh is not doing the job even when the
    /// completion figure looks survivable.
    pub fn satellite_share(&self) -> f64 {
        ratio(self.satellite, self.resolved())
    }

    /// Loss the mesh is actually responsible for — looped or out of hops.
    /// Distinct from satellite, which is a change of channel, not a loss.
    pub fn mesh_loss_rate(&self) -> f64 {
        ratio(self.dropped_loop + self.dropped_ttl, self.originated)
    }

    /// Share of delivered bundles whose receipt made it home. Its complement is
    /// the band where an origin cannot tell "never arrived" from "arrived, and
    /// the receipt died" — the belief-versus-truth gap in its purest form.
    pub fn ack_rate(&self) -> f64 {
        ratio(self.acked, self.delivered)
    }

    /// Deliveries per wake slot that actually moved something. The
    /// airtime-normalised view: two protocols can deliver equally while one
    /// spends twice the radio time doing it.
    pub fn delivered_per_slot_used(&self) -> f64 {
        ratio(self.delivered, self.slots_used())
    }

    /// Delivery rate as a fraction of what the last hop could physically pass.
    /// Below ~1 the mesh is the constraint; near 1 the tower-adjacent
    /// population is, and no amount of routing work will help.
    pub fn ceiling_utilisation(&self, params: &DvDtnParams) -> f64 {
        let ceiling = self.delivery_capacity_per_round(params);
        if self.rounds_sampled == 0 || ceiling == 0.0 {
            return 0.0;
        }
        (self.delivered as f64 / self.rounds_sampled as f64) / ceiling
    }
}

/// Guarded division, so an empty run reports zero rather than a NaN that would
/// poison every downstream mean.
fn ratio(num: u64, den: u64) -> f64 {
    if den == 0 {
        0.0
    } else {
        num as f64 / den as f64
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
