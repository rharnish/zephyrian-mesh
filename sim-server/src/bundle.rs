// Telemetry bundles — C2 of docs/design/MESH_COMMS_DESIGN.md.
//
// The counters/histograms this logic reports into (`BundleStats`, `bump`,
// `hist_mean`) live in bundle_stats.rs, re-exported here — kept separate so
// the queueing/ack/satellite-fallback logic below isn't interleaved with
// stats bookkeeping.
//
// A bundle is data a balloon wants on the ground. It gets there by being handed
// balloon to balloon along each holder's *believed* next hop — a local, possibly
// stale, possibly suboptimal choice. Nothing here consults the edge graph or the
// union-find on a balloon's behalf; a balloon can only forward to a neighbour it
// currently believes leads somewhere, and the transmission only lands if that
// neighbour is genuinely in range right now.
//
// Two rules give this its character, both from §4 of the design doc:
//
//   * **One hop per duty-cycle slot.** A bundle moves only when its holder wakes
//     to transmit, on the same slot it beacons on. A hop therefore costs
//     BEACON_INTERVAL_ROUNDS, and a d-hop path costs d times that. Near the
//     percolation threshold that exceeds a belief's lifetime, so deep bundles
//     strand — which is the designed behaviour, not a failure.
//   * **A stale next hop holds, it does not drop.** If the believed next hop is
//     no longer in range the bundle waits for the belief to refresh. That is what
//     makes this delay-tolerant rather than merely lossy. The same applies to a
//     receiver whose queue is full.
//
// Each balloon holds a bounded FIFO queue (RELAY_QUEUE_CAPACITY) and transmits
// the head of it once per duty-cycle slot. The queue is separate from the
// one-outstanding rule on origination: capping *carriage* at one meant a balloon
// holding its own bundle could not relay anyone else's, which gridlocked the
// mesh and cost roughly half of all deliveries.
//
// Slice 2 adds two things, both in this module:
//
//   * **Tower acks**, source-routed back along a bundle's recorded `path`,
//     reversed. Created the instant a bundle reaches a tower (the same
//     "instantaneous" convention tower delivery itself already uses) and then
//     hop by hop back to the origin, one balloon at a time.
//   * **Satellite fallback** in place of the old silent expiry: a bundle that
//     ages past BUNDLE_MAX_AGE_ROUNDS wherever it currently sits is handed to
//     satellite instead of dropped.
//
// Acks are not a free side channel. A relay's wake slot is one transmission,
// so an ack held by a non-tower-adjacent balloon takes priority over that
// balloon's own bundle-forwarding this slot — letting both ride the same wake
// would quietly double a balloon's per-slot throughput and undermine the
// scarce-slot model the last-hop saturation findings depend on. Tower
// contacts are unaffected: draining TOWER_CONTACT_BUNDLES already runs on a
// separate, higher-capacity link, which is why it's allowed to break the
// one-per-slot rule in the first place.
//
// The balloon's own view of its bundle (Pending/Acked/TimedOut, on
// `Balloon::outstanding`) is deliberately poorer than what these stats can
// see: satellite delivery is silent to the origin, so "never arrived",
// "arrived but the ack died", and "arrived via satellite" are all
// indistinguishable from inside, by construction. See docs/design/MESH_COMMS_DESIGN.md §4.

use crate::balloon::Balloon;
use crate::beacon::MeshAdjacency;
use crate::bundle_stats::bump;
use crate::config::*;

pub use crate::bundle_stats::{hist_mean, BundleStats};

/// One unit of telemetry in transit.
#[derive(Debug, Clone)]
pub struct Bundle {
    pub origin_id: u32,
    pub seq: u64,
    pub created_at_round: u64,
    /// What the origin measured — the copy that travels. The origin keeps its
    /// own copy in `Balloon::log`; see telemetry.rs for why there are two.
    /// Nothing about routing reads this: it is payload, carried and delivered.
    pub record: crate::telemetry::TelemetryRecord,
    /// Every balloon that has held this bundle, in order, origin first. The
    /// current holder is always `path.last()`. This single field provides loop
    /// detection, the hop budget, and the provenance that C3's relay
    /// attestations will sign.
    pub path: Vec<u32>,
}

impl Bundle {
    pub fn hops(&self) -> usize {
        self.path.len().saturating_sub(1)
    }

    pub fn age(&self, round: u64) -> u64 {
        round.saturating_sub(self.created_at_round)
    }
}

/// A tower's receipt for one bundle, source-routed back along that bundle's
/// recorded path, reversed. Carries the *bundle's* `created_at_round`, not its
/// own — a round trip has to complete inside the same age budget as the
/// one-way bundle timeout (see BUNDLE_MAX_AGE_ROUNDS), not a separate one.
#[derive(Debug, Clone)]
pub struct Ack {
    pub origin_id: u32,
    pub seq: u64,
    pub created_at_round: u64,
    /// Stops remaining after the current holder, ending at the origin. Popped
    /// as the ack advances; the pop that empties this is the one that lands it
    /// on the origin.
    pub remaining: std::collections::VecDeque<u32>,
    /// `remaining`'s length at creation — fixed, so `total_hops - remaining.len()`
    /// at any later point is how many hops the ack has completed. That's what
    /// lets a *lost* ack report how far it got instead of just that it died.
    pub total_hops: u32,
}

impl Ack {
    pub fn age(&self, round: u64) -> u64 {
        round.saturating_sub(self.created_at_round)
    }
}

/// What a balloon can conclude about its own most recently originated bundle —
/// necessarily poorer than server truth. `TimedOut` covers "never arrived",
/// "arrived but the ack died", and "arrived via satellite" alike: none of
/// those are distinguishable from inside, which is the point of the design,
/// not a gap in it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum AckState {
    Pending,
    Acked,
    TimedOut,
}

/// How a bundle actually got to a tower — server truth, not the balloon's own
/// (poorer) view. Drives the per-balloon last-delivery glyph (§3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Channel {
    Radio,
    Satellite,
}

#[derive(Debug, Clone)]
pub struct OutstandingBundle {
    pub seq: u64,
    pub created_at_round: u64,
    pub state: AckState,
    /// The bundle's full recorded path once it itself resolved (delivered to
    /// a tower, or handed to satellite) — `None` while still Pending. This is
    /// server truth kept for the C4 animated-packet view, not something the
    /// balloon could derive on its own.
    pub path: Option<Vec<u32>>,
    /// How the bundle got through, set at the same moment as `path`.
    pub channel: Option<Channel>,
    /// The tower that actually took delivery, set at the same moment as
    /// `path`/`channel`. `None` for satellite delivery and dead ends — the
    /// bundle never reached a tower in either case.
    pub tower_id: Option<u32>,
    /// How many hops of the reverse ack path completed before it was lost —
    /// only meaningful once `state == TimedOut` and the bundle *did* reach a
    /// tower (an ack existed to lose). `None` if the bundle itself never
    /// arrived (no ack was ever spawned) or the ack made it all the way.
    pub ack_hops_completed: Option<u32>,
}

/// A settled copy of `OutstandingBundle`, taken the moment its state stops
/// being `Pending`. See `Balloon::last_resolved` for why this needs to exist
/// separately from `outstanding` itself.
#[derive(Debug, Clone)]
pub struct ResolvedBundle {
    pub seq: u64,
    pub state: AckState,
    pub channel: Option<Channel>,
    pub tower_id: Option<u32>,
    pub path: Vec<u32>,
    pub ack_hops_completed: Option<u32>,
}

/// Called right after `b.outstanding`'s state flips off `Pending`, to freeze
/// a copy in `b.last_resolved` before the *next* origination overwrites
/// `outstanding` with a fresh `Pending` one. A no-op if `outstanding` is
/// absent or still `Pending` (nothing settled yet).
fn snapshot_resolved(b: &mut Balloon) {
    let Some(o) = &b.outstanding else { return };
    if o.state == AckState::Pending {
        return;
    }
    let (seq, state, channel, tower_id, path, ack_hops_completed) = (
        o.seq,
        o.state,
        o.channel,
        o.tower_id,
        o.path.clone().unwrap_or_default(),
        o.ack_hops_completed,
    );

    // Also stamp the matching retained telemetry record, so the C4 comms-log
    // panel (§3) can show a full history of outcomes, not just the latest —
    // `last_resolved` below only ever holds one.
    if let Some(record) = b.log.iter_mut().find(|r| r.seq == seq) {
        record.channel = channel;
        record.tower_id = tower_id;
        record.ack_state = state;
        record.hops = if path.is_empty() { None } else { Some(path.len() as u32 - 1) };
        record.ack_hops_completed = ack_hops_completed;
    }

    b.last_resolved =
        Some(ResolvedBundle { seq, state, channel, tower_id, path, ack_hops_completed });
}

/// Records how far a bundle got before it was destroyed in the mesh (loop or
/// TTL — never reached a tower, so it's not a satellite handoff either). The
/// origin's own view still only learns `TimedOut` once it ages out (see 1b);
/// this is the server-truth path the C4 animated-packet view reads, same
/// idiom as the satellite/delivered cases above.
fn record_dead_end(balloons: &mut [Balloon], origin_id: u32, seq: u64, path: Vec<u32>) {
    if let Some(o) = balloons.get_mut(origin_id as usize).and_then(|b| b.outstanding.as_mut()) {
        if o.seq == seq {
            o.path = Some(path);
        }
    }
}

/// Advance bundles by one comms round.
///
/// `awake` is the set of balloon indices that transmitted this round, taken
/// straight from `beacon::step` — sharing that set is what ties forwarding to
/// the radio duty cycle rather than giving bundles a schedule of their own.
pub fn step(
    balloons: &mut [Balloon],
    adj: &MeshAdjacency,
    awake: &[usize],
    round: u64,
    stats: &mut BundleStats,
) {
    // 1. Expire bundles. One that's aged past its budget wherever it currently
    //    sits is handed to satellite rather than dropped.
    let mut satellite_origins: Vec<(u32, u64, Vec<u32>)> = Vec::new(); // (origin_id, seq, path)
    for b in balloons.iter_mut() {
        let before = b.queue.len();
        for bd in b.queue.iter() {
            if bd.age(round) > BUNDLE_MAX_AGE_ROUNDS {
                bump(&mut stats.satellite_hops, bd.hops());
                satellite_origins.push((bd.origin_id, bd.seq, bd.path.clone()));
            }
        }
        b.queue.retain(|bd| bd.age(round) <= BUNDLE_MAX_AGE_ROUNDS);
        stats.satellite += (before - b.queue.len()) as u64;
    }
    for (origin_id, seq, path) in satellite_origins {
        if let Some(o) = balloons.get_mut(origin_id as usize) {
            o.last_channel = Some(Channel::Satellite);
            if let Some(out) = o.outstanding.as_mut() {
                if out.seq == seq {
                    out.path = Some(path);
                    out.channel = Some(Channel::Satellite);
                }
            }
        }
    }

    // 1a. Expire acks. Keyed on the *bundle's* created_at_round (carried
    //     verbatim on the ack), so a round trip shares the bundle's own age
    //     budget rather than getting a separate clock.
    let mut ack_died: Vec<(u32, u64, u32)> = Vec::new(); // (origin_id, seq, hops_completed)
    for b in balloons.iter_mut() {
        let before = b.ack_queue.len();
        for a in b.ack_queue.iter() {
            if a.age(round) > BUNDLE_MAX_AGE_ROUNDS {
                ack_died.push((a.origin_id, a.seq, a.total_hops - a.remaining.len() as u32));
            }
        }
        b.ack_queue.retain(|a| a.age(round) <= BUNDLE_MAX_AGE_ROUNDS);
        stats.ack_lost += (before - b.ack_queue.len()) as u64;
    }
    for (origin_id, seq, hops) in ack_died {
        if let Some(o) = balloons.get_mut(origin_id as usize).and_then(|b| b.outstanding.as_mut())
        {
            if o.seq == seq {
                o.ack_hops_completed = Some(hops);
            }
        }
    }

    // 1b. A balloon still waiting past the timeout gives up on hearing back.
    //     TimedOut is deliberately the same outcome whether the bundle never
    //     arrived, arrived and the ack died, or arrived via satellite — none of
    //     those are distinguishable from inside.
    for b in balloons.iter_mut() {
        if let Some(o) = b.outstanding.as_mut() {
            if o.state == AckState::Pending && round.saturating_sub(o.created_at_round) > BUNDLE_MAX_AGE_ROUNDS
            {
                o.state = AckState::TimedOut;
                snapshot_resolved(b);
            }
        }
    }

    // 1c. Sample the last-hop population. This is the delivery bottleneck, so
    //     it gets measured every round rather than inferred from geometry.
    stats.rounds_sampled += 1;
    stats.tower_adjacent_samples +=
        (0..balloons.len()).filter(|&i| adj.tower_in_range(i).is_some()).count() as u64;

    // 2. Ack-forwarding. Takes priority over bundle-forwarding *to a mesh
    //    peer* on a shared wake slot: that hop is one transmission, and letting
    //    an ack ride alongside a bundle-forward would quietly double a
    //    balloon's per-slot throughput there. This does NOT compete with tower
    //    draining below — the tower link is a separate, higher-capacity
    //    channel (same reason TOWER_CONTACT_BUNDLES already breaks the
    //    one-per-slot rule), and a tower-adjacent balloon's belief never
    //    points at a mesh peer anyway (hop_count == 1, next_hop == None), so it
    //    was never a candidate for the bundle-forward branch this contends
    //    with — excluding it here would only strand acks it's relaying for
    //    someone else. Two-phase gather/apply, same reason as bundle-forwarding
    //    below.
    let mut ack_used: std::collections::HashSet<usize> = std::collections::HashSet::new();
    let mut ack_moves: Vec<(u32, Ack)> = Vec::new();
    let mut ack_resolved: Vec<(u32, u64)> = Vec::new(); // (origin_id, seq)

    for &i in awake {
        let Some(ack) = balloons[i].ack_queue.front() else { continue };
        let Some(&next) = ack.remaining.front() else { continue };
        if !adj.is_neighbor(i, next) {
            continue; // stale hop — hold, same delay-tolerant idiom as bundles
        }
        let mut ack = balloons[i].ack_queue.pop_front().expect("checked front");
        ack.remaining.pop_front();
        ack_used.insert(i);
        if ack.remaining.is_empty() {
            ack_resolved.push((ack.origin_id, ack.seq));
        } else {
            ack_moves.push((next, ack));
        }
    }

    for (to, ack) in ack_moves {
        let to_idx = to as usize;
        let has_room =
            balloons.get(to_idx).is_some_and(|b| b.ack_queue.len() < ACK_QUEUE_CAPACITY);
        if has_room {
            balloons[to_idx].ack_queue.push_back(ack);
        } else {
            stats.ack_lost += 1;
            let hops = ack.total_hops - ack.remaining.len() as u32;
            if let Some(o) =
                balloons.get_mut(ack.origin_id as usize).and_then(|b| b.outstanding.as_mut())
            {
                if o.seq == ack.seq {
                    o.ack_hops_completed = Some(hops);
                }
            }
        }
    }

    for (origin_id, seq) in ack_resolved {
        stats.acked += 1;
        if let Some(b) = balloons.get_mut(origin_id as usize) {
            if let Some(o) = b.outstanding.as_mut() {
                if o.seq == seq && o.state == AckState::Pending {
                    o.state = AckState::Acked;
                }
            }
            snapshot_resolved(b);
        }
    }

    // 3. Gather this round's bundle transmissions. Two-phase for the same
    //    reason as beacon::step — applying in place would let a bundle race
    //    along several hops within one round depending on iteration order,
    //    collapsing exactly the delay being modelled.
    let mut moves: Vec<(usize, u32)> = Vec::new(); // (from index, to balloon id)

    // One transmission per awake radio, so only the head of the queue moves.
    // Tower draining below is never in contention with an ack this balloon is
    // relaying — see the note on `ack_used` above — so it isn't gated on it;
    // only the mesh-forward arm (`Some(next)`) is.
    for &i in awake {
        if let Some(bel) = balloons[i].belief {
            bump(&mut stats.belief_hops, bel.hop_count as usize);
        }
        let Some(bundle) = balloons[i].queue.front() else { continue };

        // A balloon with no belief has nowhere to send it. Hold.
        let Some(belief) = balloons[i].belief else {
            stats.slots_with_bundle += 1;
            stats.stall_no_belief += 1;
            continue;
        };

        match belief.next_hop {
            // Believes it hears a tower directly. Only true if the link is
            // still live — belief can be stale, the radio cannot lie.
            None => {
                stats.slots_with_bundle += 1;
                if let Some(tower_id) = adj.tower_in_range(i) {
                    // A tower contact drains up to TOWER_CONTACT_BUNDLES, not
                    // one. The one-per-slot rule rations *beacon* airtime; a
                    // point-to-point link to a ground station is a different
                    // event, and this is the only lever that acts on the last
                    // hop, which is where the throughput limit actually lives.
                    let n = TOWER_CONTACT_BUNDLES.min(balloons[i].queue.len());
                    for _ in 0..n {
                        let bd = balloons[i].queue.pop_front().expect("checked non-empty");
                        bump(&mut stats.delivered_hops, bd.hops());
                        stats.delivered += 1;
                        if let Some(o) = balloons.get_mut(bd.origin_id as usize) {
                            o.last_channel = Some(Channel::Radio);
                            if let Some(out) = o.outstanding.as_mut() {
                                if out.seq == bd.seq {
                                    out.path = Some(bd.path.clone());
                                    out.channel = Some(Channel::Radio);
                                    out.tower_id = Some(tower_id);
                                }
                            }
                        }

                        // Spawn the ack, source-routed back along the reversed
                        // path. A zero-hop bundle (the origin delivered
                        // directly) resolves on the spot — there's no one to
                        // hand a packet to.
                        let mut reversed: std::collections::VecDeque<u32> =
                            bd.path.iter().rev().copied().collect();
                        reversed.pop_front(); // drop `i` itself — already here
                        if reversed.is_empty() {
                            stats.acked += 1;
                            if let Some(o) = balloons[i].outstanding.as_mut() {
                                if o.seq == bd.seq && o.state == AckState::Pending {
                                    o.state = AckState::Acked;
                                }
                            }
                            snapshot_resolved(&mut balloons[i]);
                        } else {
                            let total_hops = reversed.len() as u32;
                            let ack = Ack {
                                origin_id: bd.origin_id,
                                seq: bd.seq,
                                created_at_round: bd.created_at_round,
                                remaining: reversed,
                                total_hops,
                            };
                            if balloons[i].ack_queue.len() < ACK_QUEUE_CAPACITY {
                                balloons[i].ack_queue.push_back(ack);
                            } else {
                                stats.ack_lost += 1;
                                if let Some(o) = balloons
                                    .get_mut(bd.origin_id as usize)
                                    .and_then(|b| b.outstanding.as_mut())
                                {
                                    if o.seq == bd.seq {
                                        o.ack_hops_completed = Some(0);
                                    }
                                }
                            }
                        }
                    }
                } else {
                    // Otherwise hold: the belief will expire or refresh.
                    stats.stall_tower_gone += 1;
                }
            }
            Some(next) => {
                if ack_used.contains(&i) {
                    // This slot's one mesh transmission went to the ack
                    // instead; the bundle just holds. Excluded from
                    // slots_with_bundle entirely rather than added to a stall
                    // bucket — the forwarding logic below never even ran.
                    continue;
                }
                stats.slots_with_bundle += 1;
                if !adj.is_neighbor(i, next) {
                    stats.stall_stale_next_hop += 1;
                    continue; // stale next hop — hold, don't drop
                }
                if bundle.path.contains(&next) {
                    let (origin_id, seq, path) = (bundle.origin_id, bundle.seq, bundle.path.clone());
                    balloons[i].queue.pop_front();
                    stats.dropped_loop += 1;
                    record_dead_end(balloons, origin_id, seq, path);
                    continue;
                }
                if bundle.path.len() >= BUNDLE_MAX_HOPS {
                    let (origin_id, seq, path) = (bundle.origin_id, bundle.seq, bundle.path.clone());
                    balloons[i].queue.pop_front();
                    stats.dropped_ttl += 1;
                    record_dead_end(balloons, origin_id, seq, path);
                    continue;
                }
                moves.push((i, next));
            }
        }
    }

    // 4. Apply. A receiver whose queue is full has nowhere to put it, so the
    //    handoff fails and the sender keeps carrying — the same hold-don't-drop
    //    rule as a stale next hop. Dropping here instead loses the overwhelming
    //    majority of traffic the moment the mesh is busy, which is what the
    //    first run of bundle_delivery.rs showed: 18270 of 21634 bundles
    //    destroyed by congestion alone.
    for (from, to) in moves {
        let to_idx = to as usize;
        let has_room =
            balloons.get(to_idx).is_some_and(|r| r.queue.len() < RELAY_QUEUE_CAPACITY);
        if !has_room {
            stats.blocked += 1;
            continue;
        }
        let Some(mut bundle) = balloons[from].queue.pop_front() else { continue };
        bundle.path.push(to);
        balloons[to_idx].queue.push_back(bundle);
    }

    // 5. Originate. Two independent limits: the queue must have room (shared
    //    with transit traffic), and the balloon may have only *one of its own*
    //    bundles outstanding — the rule the relay queue was wrongly enforcing.
    for &i in awake {
        let b = &mut balloons[i];
        if round < b.next_bundle_round || b.queue.len() >= RELAY_QUEUE_CAPACITY {
            continue;
        }
        if b.queue.iter().any(|bd| bd.origin_id == b.id) {
            continue; // own bundle still in hand
        }
        // Measure once, then keep one copy and send the other. Both carry the
        // same seq because they are the same event (see telemetry.rs).
        let record = crate::telemetry::TelemetryRecord::sample(b, round);
        if b.log.len() >= COMMS_LOG_CAPACITY {
            b.log.pop_front();
        }
        b.log.push_back(record.clone());
        b.queue.push_back(Bundle {
            origin_id: b.id,
            seq: b.bundle_seq,
            created_at_round: round,
            record,
            path: vec![b.id],
        });
        b.outstanding = Some(OutstandingBundle {
            seq: b.bundle_seq,
            created_at_round: round,
            state: AckState::Pending,
            path: None,
            channel: None,
            tower_id: None,
            ack_hops_completed: None,
        });
        b.bundle_seq += 1;
        b.next_bundle_round = round + BUNDLE_INTERVAL_ROUNDS;
        stats.originated += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::beacon::RouteBelief;
    use crate::link_detection::{Edge, NodeKey};
    use crate::tower::Tower;

    fn node_key(s: &str) -> NodeKey {
        let (tag, rest) = s.split_at(1);
        let id: u32 = rest.parse().unwrap();
        match tag {
            "b" => NodeKey::Balloon(id),
            "t" => NodeKey::Tower(id),
            _ => panic!("bad test node key {s:?}"),
        }
    }

    fn edge(a: &str, b: &str) -> Edge {
        Edge { a: node_key(a), b: node_key(b) }
    }

    /// Payload for hand-built test bundles. Routing never reads the record, so
    /// these tests only need it to exist and to carry the right origin/seq.
    fn test_record(origin_id: u32, seq: u64) -> crate::telemetry::TelemetryRecord {
        let mut b = Balloon::new(origin_id, 0.0, 0.0, 18_000.0);
        b.bundle_seq = seq;
        crate::telemetry::TelemetryRecord::sample(&b, 0)
    }

    fn line(n: usize) -> (Vec<Balloon>, Vec<Tower>, MeshAdjacency) {
        // t0 -- b0 -- b1 -- ... -- b(n-1)
        let balloons: Vec<Balloon> =
            (0..n).map(|i| Balloon::new(i as u32, i as f64, 0.0, 18000.0)).collect();
        let towers = vec![Tower::new(0, -1.0, 0.0, 30.0)];
        let mut edges = vec![edge("t0", "b0")];
        for i in 0..n.saturating_sub(1) {
            edges.push(edge(&format!("b{i}"), &format!("b{}", i + 1)));
        }
        let mut adj = MeshAdjacency::default();
        adj.rebuild(&edges, n, &towers);
        (balloons, towers, adj)
    }

    /// Give every balloon a correct belief pointing one hop closer to the tower.
    fn seed_beliefs(balloons: &mut [Balloon], round: u64) {
        for i in 0..balloons.len() {
            balloons[i].belief = Some(RouteBelief {
                tower_id: 0,
                hop_count: i as u32 + 1,
                next_hop: if i == 0 { None } else { Some(i as u32 - 1) },
                epoch: 1,
                emitted_at_round: round,
            });
        }
    }

    #[test]
    fn a_bundle_walks_the_chain_and_is_delivered() {
        let (mut balloons, _t, adj) = line(4);
        seed_beliefs(&mut balloons, 0);
        let mut stats = BundleStats::default();
        let awake: Vec<usize> = (0..4).collect();

        // Only b3 originates; the rest just relay.
        for b in balloons.iter_mut() {
            b.next_bundle_round = u64::MAX;
        }
        balloons[3].next_bundle_round = 0;

        for round in 0..10 {
            step(&mut balloons, &adj, &awake, round, &mut stats);
            if stats.delivered > 0 {
                break;
            }
        }
        assert_eq!(stats.delivered, 1, "stats: {stats:?}");
        assert_eq!(stats.dropped_loop + stats.dropped_ttl + stats.satellite, 0);

        // The origin's own retained record now carries the recorded path and
        // the channel it went out on — the data the C4 animated-packet view
        // reads (docs/design/MESH_COMMS_DESIGN.md §3).
        let outstanding = balloons[3].outstanding.as_ref().unwrap();
        assert_eq!(outstanding.channel, Some(Channel::Radio));
        assert_eq!(outstanding.path.as_deref(), Some(&[3, 2, 1, 0][..]));
        // `line()` wires exactly one tower at id 0 — the path stops one hop
        // short of it, so the animated-packet view needs this recorded
        // separately to draw the final leg.
        assert_eq!(outstanding.tower_id, Some(0));
    }

    #[test]
    fn path_history_records_every_relay_in_order() {
        let (mut balloons, _t, adj) = line(4);
        seed_beliefs(&mut balloons, 0);
        let mut stats = BundleStats::default();
        let awake: Vec<usize> = (0..4).collect();
        for b in balloons.iter_mut() {
            b.next_bundle_round = u64::MAX;
        }
        balloons[3].next_bundle_round = 0;

        step(&mut balloons, &adj, &awake, 0, &mut stats); // b3 originates
        assert_eq!(balloons[3].queue.front().unwrap().path, vec![3]);

        step(&mut balloons, &adj, &awake, 1, &mut stats); // 3 -> 2
        assert_eq!(balloons[2].queue.front().unwrap().path, vec![3, 2]);

        step(&mut balloons, &adj, &awake, 2, &mut stats); // 2 -> 1
        let held = balloons[1].queue.front().unwrap();
        assert_eq!(held.path, vec![3, 2, 1]);
        assert_eq!(held.hops(), 2);
    }

    /// The delay-tolerance rule: a next hop that has gone out of range makes the
    /// bundle *wait*, not vanish.
    #[test]
    fn a_stale_next_hop_holds_the_bundle_rather_than_dropping_it() {
        let (mut balloons, towers, _adj) = line(3);
        // Rebuild adjacency with the b1--b0 link missing, so b1's belief that it
        // can reach b0 is stale.
        let mut adj = MeshAdjacency::default();
        adj.rebuild(&[edge("t0", "b0"), edge("b1", "b2")], 3, &towers);
        seed_beliefs(&mut balloons, 0);

        let mut stats = BundleStats::default();
        let awake: Vec<usize> = (0..3).collect();
        for b in balloons.iter_mut() {
            b.next_bundle_round = u64::MAX;
        }
        balloons[1].queue.push_back(Bundle {
            origin_id: 1,
            seq: 0,
            created_at_round: 0,
            record: test_record(1, 0),
            path: vec![1],
        });

        for round in 0..10 {
            step(&mut balloons, &adj, &awake, round, &mut stats);
        }
        assert!(!balloons[1].queue.is_empty(), "should still be holding");
        assert_eq!(stats.resolved(), 0, "nothing should have resolved: {stats:?}");
    }

    #[test]
    fn a_bundle_offered_back_to_a_balloon_already_in_its_path_is_dropped_as_a_loop() {
        let (mut balloons, towers, _a) = line(2);
        let mut adj = MeshAdjacency::default();
        adj.rebuild(&[edge("b0", "b1")], 2, &towers);

        // b1 believes b0 is its next hop; b0 believes b1 is. A bundle handed
        // between them must not ping-pong.
        balloons[0].belief = Some(RouteBelief {
            tower_id: 0, hop_count: 3, next_hop: Some(1), epoch: 1, emitted_at_round: 0,
        });
        balloons[1].belief = Some(RouteBelief {
            tower_id: 0, hop_count: 3, next_hop: Some(0), epoch: 1, emitted_at_round: 0,
        });
        for b in balloons.iter_mut() {
            b.next_bundle_round = u64::MAX;
        }
        balloons[1].queue.push_back(Bundle {
            origin_id: 1, seq: 0, created_at_round: 0, record: test_record(1, 0), path: vec![1],
        });

        let mut stats = BundleStats::default();
        let awake: Vec<usize> = vec![0, 1];
        for round in 0..6 {
            step(&mut balloons, &adj, &awake, round, &mut stats);
        }
        assert_eq!(stats.dropped_loop, 1, "stats: {stats:?}");
        assert!(balloons.iter().all(|b| b.queue.is_empty()));
    }

    /// The whole point of the queue: a relay busy with its own bundle must still
    /// be able to accept someone else's.
    #[test]
    fn a_relay_carrying_its_own_bundle_still_accepts_transit_traffic() {
        let (mut balloons, _t, adj) = line(3);
        seed_beliefs(&mut balloons, 0);
        let mut stats = BundleStats::default();

        // b1 (the middle relay) is holding one of its own; b2 sends through it.
        // Only b2 is awake — if b1 also transmitted it would forward its own
        // bundle onward in the same step and the queue would net out at 1,
        // which says nothing about whether it accepted the transit bundle.
        for b in balloons.iter_mut() {
            b.next_bundle_round = u64::MAX;
        }
        balloons[1].queue.push_back(Bundle {
            origin_id: 1, seq: 0, created_at_round: 0, record: test_record(1, 0), path: vec![1],
        });
        balloons[2].queue.push_back(Bundle {
            origin_id: 2, seq: 0, created_at_round: 0, record: test_record(2, 0), path: vec![2],
        });

        step(&mut balloons, &adj, &[2], 1, &mut stats);
        assert_eq!(balloons[1].queue.len(), 2, "relay should have accepted transit");
        assert_eq!(stats.blocked, 0);
    }

    #[test]
    fn a_full_queue_blocks_the_handoff_without_losing_the_bundle() {
        let (mut balloons, _t, adj) = line(3);
        seed_beliefs(&mut balloons, 0);
        let mut stats = BundleStats::default();
        for b in balloons.iter_mut() {
            b.next_bundle_round = u64::MAX;
        }
        for k in 0..RELAY_QUEUE_CAPACITY {
            balloons[1].queue.push_back(Bundle {
                origin_id: 1, seq: k as u64, created_at_round: 0, record: test_record(1, k as u64), path: vec![1],
            });
        }
        balloons[2].queue.push_back(Bundle {
            origin_id: 2, seq: 0, created_at_round: 0, record: test_record(2, 0), path: vec![2],
        });

        // b2 is awake but b1 is full: the handoff fails and b2 keeps it.
        step(&mut balloons, &adj, &[2], 1, &mut stats);
        assert_eq!(stats.blocked, 1);
        assert_eq!(balloons[2].queue.len(), 1, "sender must keep the bundle");
        assert_eq!(stats.resolved(), 0, "nothing lost: {stats:?}");
    }

    #[test]
    fn a_balloon_originates_only_one_of_its_own_at_a_time() {
        let mut balloons = vec![Balloon::new(0, 0.0, 0.0, 18000.0)];
        let adj = MeshAdjacency::default();
        let mut stats = BundleStats::default();
        // Plenty of queue room and the interval always elapsed, yet only one of
        // its own may be outstanding.
        for round in 0..10 {
            balloons[0].next_bundle_round = 0;
            step(&mut balloons, &adj, &[0], round, &mut stats);
        }
        assert_eq!(stats.originated, 1, "stats: {stats:?}");
        assert_eq!(balloons[0].queue.len(), 1);
    }

    /// A tower contact drains the queue rather than dribbling one bundle per
    /// duty cycle — the last hop is the throughput limit, so this is the one
    /// place a burst is worth spending airtime on.
    #[test]
    fn a_tower_contact_drains_up_to_a_full_contact_window() {
        let (mut balloons, _t, adj) = line(2);
        seed_beliefs(&mut balloons, 0);
        let mut stats = BundleStats::default();
        for b in balloons.iter_mut() {
            b.next_bundle_round = u64::MAX;
        }
        // b0 hears the tower directly and is holding a full queue.
        for k in 0..RELAY_QUEUE_CAPACITY {
            balloons[0].queue.push_back(Bundle {
                origin_id: 1, seq: k as u64, created_at_round: 0, record: test_record(1, k as u64), path: vec![1, 0],
            });
        }

        step(&mut balloons, &adj, &[0], 1, &mut stats);

        let expected = TOWER_CONTACT_BUNDLES.min(RELAY_QUEUE_CAPACITY);
        assert_eq!(stats.delivered, expected as u64, "stats: {stats:?}");
        assert_eq!(balloons[0].queue.len(), RELAY_QUEUE_CAPACITY - expected);
    }

    /// The contact window applies only to towers. A relay handing off to another
    /// balloon still moves exactly one bundle per slot, because that transmission
    /// is rationed by the sender's duty cycle in the ordinary way.
    #[test]
    fn a_balloon_to_balloon_handoff_still_moves_only_one_bundle() {
        let (mut balloons, _t, adj) = line(3);
        seed_beliefs(&mut balloons, 0);
        let mut stats = BundleStats::default();
        for b in balloons.iter_mut() {
            b.next_bundle_round = u64::MAX;
        }
        for k in 0..4 {
            balloons[2].queue.push_back(Bundle {
                origin_id: 2, seq: k, created_at_round: 0, record: test_record(2, k), path: vec![2],
            });
        }

        step(&mut balloons, &adj, &[2], 1, &mut stats);

        assert_eq!(balloons[1].queue.len(), 1, "only one bundle should have crossed");
        assert_eq!(balloons[2].queue.len(), 3, "the rest stay with the sender");
    }

    #[test]
    fn a_bundle_nobody_can_move_eventually_goes_to_satellite() {
        // One balloon, no links, no belief: it originates and can never send.
        let mut balloons = vec![Balloon::new(0, 0.0, 0.0, 18000.0)];
        let adj = MeshAdjacency::default();
        let mut stats = BundleStats::default();

        for round in 0..(BUNDLE_MAX_AGE_ROUNDS + 5) {
            step(&mut balloons, &adj, &[0], round, &mut stats);
        }
        assert!(stats.satellite >= 1, "stats: {stats:?}");
        assert_eq!(stats.delivered, 0);
        assert_eq!(stats.resolved(), stats.satellite, "satellite counts as resolved");
        // Silent to the origin: it never hears back, so its own view times out
        // even though the bundle did get out via satellite.
        let outstanding = balloons[0].outstanding.as_ref().unwrap();
        assert_eq!(outstanding.state, AckState::TimedOut);
        // But server truth (what the C4 view reads) does know it was satellite.
        assert_eq!(outstanding.channel, Some(Channel::Satellite));
        assert_eq!(outstanding.tower_id, None, "satellite delivery never touches a tower");
        assert_eq!(outstanding.path.as_deref(), Some(&[0][..]));
        // And it's been frozen into `last_resolved`, which is what the query
        // endpoint actually serves — `outstanding` alone would reset to
        // Pending the moment this balloon originates its next bundle.
        let resolved = balloons[0].last_resolved.as_ref().unwrap();
        assert_eq!(resolved.channel, Some(Channel::Satellite));
        assert_eq!(resolved.tower_id, None);
        assert_eq!(resolved.path, vec![0]);
    }

    /// Originating measures the atmosphere once and keeps both copies in step:
    /// one retained in the log, one riding the bundle.
    #[test]
    fn originating_records_telemetry_in_the_log_and_in_the_bundle() {
        let mut balloons = vec![Balloon::new(0, 10.0, 20.0, 18_000.0)];
        let adj = MeshAdjacency::default();
        let mut stats = BundleStats::default();

        step(&mut balloons, &adj, &[0], 5, &mut stats);

        assert_eq!(balloons[0].log.len(), 1);
        let logged = balloons[0].log.front().unwrap();
        let carried = &balloons[0].queue.front().unwrap().record;

        assert_eq!(logged.seq, 0);
        assert_eq!(logged.seq, carried.seq, "the two copies are the same event");
        assert_eq!(logged.created_at_round, 5);
        assert_eq!(logged.alt_m, 18_000.0, "must sample where the balloon actually was");
        assert!((logged.temperature_k - crate::atmosphere::T11).abs() < 1e-9);
        assert!(logged.pressure_hpa > 0.0);
    }

    /// The log is bounded — an unbounded one is the kind of leak that looks
    /// fine in a short harness run and eats memory in a server left up.
    #[test]
    fn the_telemetry_log_stays_bounded() {
        let mut balloons = vec![Balloon::new(0, 0.0, 0.0, 18_000.0)];
        let adj = MeshAdjacency::default();
        let mut stats = BundleStats::default();

        // Originate far more than the log can hold. The queue is drained each
        // round so the one-outstanding rule never blocks origination.
        for round in 0..(COMMS_LOG_CAPACITY as u64 * 3) {
            balloons[0].next_bundle_round = 0;
            balloons[0].queue.clear();
            step(&mut balloons, &adj, &[0], round, &mut stats);
        }

        assert_eq!(balloons[0].log.len(), COMMS_LOG_CAPACITY);
        // It kept the newest, not the oldest.
        let seqs: Vec<u64> = balloons[0].log.iter().map(|r| r.seq).collect();
        assert!(seqs.windows(2).all(|w| w[0] < w[1]), "log must stay ordered: {seqs:?}");
        assert_eq!(*seqs.last().unwrap(), stats.originated - 1);
    }

    /// Happy path: a delivered bundle's ack walks the whole reverse path and
    /// the origin's own view flips to Acked.
    #[test]
    fn an_ack_completes_the_reverse_path_and_the_origin_learns_it() {
        let (mut balloons, _t, adj) = line(4);
        seed_beliefs(&mut balloons, 0);
        let mut stats = BundleStats::default();
        let awake: Vec<usize> = (0..4).collect();
        for b in balloons.iter_mut() {
            b.next_bundle_round = u64::MAX;
        }
        balloons[3].next_bundle_round = 0;

        // Run long enough for the bundle to reach the tower (b0) and the ack
        // to walk all the way back to b3 (origin).
        for round in 0..40 {
            step(&mut balloons, &adj, &awake, round, &mut stats);
            if balloons[3].outstanding.as_ref().is_some_and(|o| o.state == AckState::Acked) {
                break;
            }
        }
        assert_eq!(stats.delivered, 1, "stats: {stats:?}");
        assert_eq!(stats.acked, 1, "stats: {stats:?}");
        assert_eq!(stats.ack_lost, 0, "stats: {stats:?}");
        assert_eq!(balloons[3].outstanding.as_ref().unwrap().state, AckState::Acked);
        assert!(balloons.iter().all(|b| b.ack_queue.is_empty()), "ack should have fully drained");
        // The matching retained telemetry record is stamped too — the C4
        // comms-log panel reads this per-row, not just `last_resolved`.
        let record = balloons[3].log.iter().find(|r| r.seq == 0).unwrap();
        assert_eq!(record.ack_state, AckState::Acked);
        assert_eq!(record.channel, Some(Channel::Radio));
        assert_eq!(record.tower_id, Some(0));
        assert_eq!(record.hops, Some(3));
    }

    /// A bundle can arrive even though its ack never makes it back — the
    /// reverse path can break after the forward trip succeeded. The origin
    /// cannot tell this apart from "never arrived": both look like TimedOut.
    #[test]
    fn an_ack_can_be_lost_even_though_delivery_succeeded() {
        let (mut balloons, towers, _adj) = line(4);
        seed_beliefs(&mut balloons, 0);
        let mut stats = BundleStats::default();
        let awake: Vec<usize> = (0..4).collect();
        for b in balloons.iter_mut() {
            b.next_bundle_round = u64::MAX;
        }
        balloons[3].next_bundle_round = 0;

        // Deliver the bundle to the tower first...
        let mut adj = MeshAdjacency::default();
        adj.rebuild(
            &[edge("t0", "b0"), edge("b0", "b1"), edge("b1", "b2"), edge("b2", "b3")],
            4,
            &towers,
        );
        let mut round = 0u64;
        while stats.delivered == 0 && round < 20 {
            step(&mut balloons, &adj, &awake, round, &mut stats);
            round += 1;
        }
        assert_eq!(stats.delivered, 1, "bundle should have reached the tower");

        // ...then sever the link the ack needs for its very next hop, before it
        // can leave b0.
        adj.rebuild(&[edge("t0", "b0"), edge("b1", "b2"), edge("b2", "b3")], 4, &towers);

        for r in round..(round + BUNDLE_MAX_AGE_ROUNDS + 10) {
            step(&mut balloons, &adj, &awake, r, &mut stats);
        }
        assert_eq!(stats.acked, 0, "stats: {stats:?}");
        assert_eq!(stats.ack_lost, 1, "stats: {stats:?}");
        let outstanding = balloons[3].outstanding.as_ref().unwrap();
        assert_eq!(outstanding.state, AckState::TimedOut);
        // The ack died on its very first hop (b0 could never reach b1) — the
        // C4 view should be able to show it dying right at the tower end,
        // not partway or at the origin.
        assert_eq!(outstanding.ack_hops_completed, Some(0));
        let resolved = balloons[3].last_resolved.as_ref().unwrap();
        assert_eq!(resolved.ack_hops_completed, Some(0));
        assert_eq!(resolved.channel, Some(Channel::Radio));
        assert_eq!(resolved.tower_id, Some(0));
    }

    /// A bundle originated and delivered with zero hops resolves on the spot —
    /// there's no reverse path to walk, so no ack packet is even created.
    #[test]
    fn a_zero_hop_delivery_acks_immediately_with_no_packet() {
        let (mut balloons, _t, adj) = line(2);
        seed_beliefs(&mut balloons, 0);
        let mut stats = BundleStats::default();
        for b in balloons.iter_mut() {
            b.next_bundle_round = u64::MAX;
        }
        balloons[0].next_bundle_round = 0; // b0 originates and hears the tower directly

        // Origination happens last within a round, so the bundle isn't in the
        // queue to deliver until b0's *next* wake slot.
        step(&mut balloons, &adj, &[0], 0, &mut stats);
        step(&mut balloons, &adj, &[0], 1, &mut stats);

        assert_eq!(stats.delivered, 1, "stats: {stats:?}");
        assert_eq!(stats.acked, 1, "stats: {stats:?}");
        assert_eq!(balloons[0].outstanding.as_ref().unwrap().state, AckState::Acked);
        assert!(balloons[0].ack_queue.is_empty(), "no packet should have been created");
    }

    /// A full ack_queue drops the incoming ack rather than blocking the
    /// handoff — the same deliberately-lossy behaviour the design calls out,
    /// not a hold-and-retry rule acks don't need.
    #[test]
    fn a_full_ack_queue_drops_the_incoming_ack() {
        let (mut balloons, towers, _a) = line(2);
        let mut adj = MeshAdjacency::default();
        adj.rebuild(&[edge("b0", "b1")], 2, &towers);
        balloons[1].belief = Some(RouteBelief {
            tower_id: 0, hop_count: 1, next_hop: Some(0), epoch: 1, emitted_at_round: 0,
        });

        for k in 0..ACK_QUEUE_CAPACITY {
            balloons[0].ack_queue.push_back(Ack {
                origin_id: 9,
                seq: k as u64,
                created_at_round: 0,
                remaining: std::collections::VecDeque::from(vec![1]),
                total_hops: 1,
            });
        }
        // One more ack arrives at b0's neighbour b1's expense: b1 forwards an
        // ack destined through b0, but b0's queue is already full.
        balloons[1].ack_queue.push_back(Ack {
            origin_id: 9,
            seq: 99,
            created_at_round: 0,
            remaining: std::collections::VecDeque::from(vec![0, 1]),
            total_hops: 2,
        });

        let mut stats = BundleStats::default();
        step(&mut balloons, &adj, &[1], 1, &mut stats);

        assert_eq!(stats.ack_lost, 1, "stats: {stats:?}");
        assert_eq!(balloons[0].ack_queue.len(), ACK_QUEUE_CAPACITY);
    }
}
