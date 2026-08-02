// Tunables belonging to the dv-dtn protocol specifically, as against
// `config.rs`, which keeps physics, link detection, and the shared comms clock
// (COMMS_EVERY_N_TICKS / COMMS_ROUND_SIM_SECONDS — a different protocol still
// needs *a* clock, just not necessarily these values).
//
// These were plain `pub const`s in config.rs. They are fields now because they
// are the cheap end of "swap in a different protocol": most of the interesting
// comparisons available today are not new families at all but variations of
// this one — a different route metric, a different queue discipline, a
// different origination rate — and those want to vary per `World`, not per
// build.
//
// `Default` returns exactly the shipped values, so a `World` built without
// saying otherwise behaves as it always has.

/// When route discovery happens.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Discovery {
    /// Towers flood beacons continuously; a balloon usually has a route
    /// already, and it may be stale. What ships.
    #[default]
    Proactive,
    /// A balloon asks only when it has a bundle and no route (AODV-style).
    /// Nothing is spent until there is something to send, but the first hop
    /// waits for a flood out and a reply back — under a duty cycle, that is
    /// hops x the wake interval in each direction.
    Reactive,
    /// Balloons gossip *observations* — who they can hear — and each computes
    /// its own route from the map it assembles. The only variant where a
    /// balloon holds a picture of the mesh rather than a single distance, and
    /// so the only one that can tell "no path exists" from "nothing heard
    /// lately".
    LinkState,
}

/// Who is allowed to answer a route request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReplyPolicy {
    /// Any node that already holds a live route answers on the requester's
    /// behalf, which is what AODV does and why it is cheap: most requests die
    /// a hop or two out instead of reaching the destination.
    ///
    /// It relies on the requester being able to tell a good answer from a bad
    /// one. Real AODV does that with destination sequence numbers. Here the
    /// equivalent is that a reply carries the age of the news it is built on
    /// (see `Rrep::emitted_at_round`) rather than the moment it was sent.
    #[default]
    Intermediate,
    /// Only nodes that can hear a tower directly may answer. Every route is
    /// then built from first-hand knowledge and cannot be longer than the
    /// flood that found it, at the cost of every request having to reach the
    /// edge of the mesh.
    TowerAdjacent,
}

/// Which route a balloon prefers when two offers compete.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Metric {
    /// Freshness first, hop count only as a tiebreak within one wave. What
    /// ships. Note this makes the protocol closer to sequence-numbered
    /// flooding (DSDV/OSPF LSA semantics) than to shortest-path
    /// distance-vector: a fresher wave wins however much longer its path.
    #[default]
    FreshestFirst,
    /// Hop count first, freshness only as a tiebreak. Measured (see
    /// docs/design/MESH_COMMS_DESIGN.md §4): helps *below* percolation
    /// (32.1% -> 39.3%) and **hurts above it** (56.0% -> 50.5%), which is
    /// where the mesh normally sits. Kept as a comparison, not as a fix.
    NearestFirst,
}

/// How many bundles ride one transmission, per kind of link.
///
/// Aggregation here is a *transmission-time* decision, not a change to bundle
/// identity: one wake slot carries K bundles, each keeping its own path,
/// origin and seq. That matters — it leaves per-record provenance (what C3's
/// signing will need) untouched, and every existing counter keeps its meaning,
/// since `delivered` still counts records either way.
///
/// This generalizes what tower contacts already did. `{ mesh_hop: 1,
/// tower_contact: 4 }` is exactly the shipped behaviour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BatchPolicy {
    /// Bundles per balloon-to-balloon hop. One is the conservative reading of
    /// a duty-cycled radio: a wake is one transmission. Raising it asks
    /// whether the mesh is limited by airtime or by opportunity.
    pub mesh_hop: usize,
    /// Bundles per tower contact. Already 4 rather than 1, because a
    /// point-to-point link to a mains-powered ground station is a different
    /// event from a broadcast beacon — see the measurements below.
    pub tower_contact: usize,
}

impl Default for BatchPolicy {
    fn default() -> Self {
        BatchPolicy { mesh_hop: 1, tower_contact: 4 }
    }
}

/// How a delivery gets acknowledged back to its origin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AckPolicy {
    /// What ships: the tower creates a receipt and it is source-routed back
    /// along the bundle's recorded path, one hop per wake slot, competing with
    /// ordinary forwarding for those slots (and winning, which is why it costs
    /// throughput).
    #[default]
    SourceRouted,
    /// Towers instead announce recent deliveries inside the beacons they are
    /// already sending, and the announcement floods outward with the wave.
    ///
    /// This is close to DTN's Aggregate Custody Signals, and it attacks the
    /// same scarcity everything else here does: the receipt costs *no
    /// additional transmissions at all*, because it rides ones that were
    /// happening anyway. One tower transmission can also satisfy many origins
    /// at once — it is a broadcast medium, so everyone in earshot learns
    /// together.
    ///
    /// Truncation is lossy in one direction only. An origin whose entry didn't
    /// fit in `ack_digest_entries` simply doesn't learn *yet*; it never learns
    /// something false. A Bloom filter would be smaller and would introduce
    /// false positives — a balloon concluding it was acked when it wasn't —
    /// which would be a genuinely new category of belief error. Interesting,
    /// but not what this option is.
    Digest,
}

/// Which held bundle a balloon transmits when its slot comes up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum QueueDiscipline {
    /// Oldest first. What ships.
    #[default]
    Fifo,
    /// Newest first. A known result in delay-tolerant networking: under load
    /// with an age-based drop rule, serving the newest first means the bundles
    /// that do get sent are the ones with the most budget left, so fewer die
    /// mid-path — at the cost of starving whatever is at the bottom.
    Lifo,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DvDtnParams {
    // --- Beacon-based connectivity discovery (design doc §1) -----------------
    //
    // What makes discovery slow here is radio *duty cycling*, not propagation
    // delay: links survive for hours of sim time (a balloon drifts ~0.4% of
    // link range per link round), so if nodes transmitted continuously every
    // belief would instantly equal ground truth and there'd be nothing to
    // simulate. Real HAB radios can't afford that — they wake, beacon, sleep.
    /// Nominal gap between a node's beacon transmissions, in comms rounds.
    pub beacon_interval_rounds: u64,
    /// +/- jitter on that gap, so nodes don't fall into lockstep.
    pub beacon_jitter_rounds: u64,
    /// Maximum age of the *news*, measured from when the tower emitted the
    /// wave — not from when this balloon last heard it repeated. Keying expiry
    /// on when a belief was last heard does not work: any relay of stale news
    /// renews its lease, so a cluster cut off from every tower sustains dead
    /// routes forever (see the beacon.rs tests, and bin/beacon_convergence.rs
    /// phase 3).
    ///
    /// This is OSPF's LSA MaxAge idea. The floor on it is propagation time: a
    /// belief has to survive long enough to reach the far end of the mesh, or
    /// deep balloons expire it on arrival and can never hold a route. Measured
    /// convergence is ~38 rounds to reach 99% of a 1200-balloon field, so this
    /// must sit above that — it is ~12 beacon intervals, not the 3-4 a
    /// contact-recency timeout would want. **Not a pacing dial**: lower it
    /// toward the convergence figure and deep balloons expire beliefs on
    /// arrival. Retune the comms clock instead.
    pub belief_max_age_rounds: u64,
    /// Stop rebroadcasting past this depth. Paths this long only occur near
    /// the percolation threshold, where handing off to satellite is the right
    /// policy anyway (see mesh_depth.rs). Also bounds distance-vector
    /// count-to-infinity.
    pub beacon_max_hops: u32,
    /// How a balloon chooses between competing route offers.
    pub metric: Metric,
    /// Whether routes are maintained continuously or fetched on demand.
    pub discovery: Discovery,
    /// Who may answer a route request. Only consulted under
    /// `Discovery::Reactive`.
    pub reply_policy: ReplyPolicy,
    /// Under `Discovery::LinkState`, how many observations ride one
    /// transmission — this balloon's own, plus relayed ones. The same
    /// information-per-transmission dial as `BatchPolicy`, applied to gossip
    /// rather than to bundles: a wake is still one transmission, and this
    /// says how much it carries. 1 would mean a balloon never relays anyone
    /// else's observation and no map could form.
    pub lsa_per_transmission: usize,

    // --- Telemetry bundles (C2, design doc §1 and §4) ------------------------
    //
    // Bundles advance one hop per *duty-cycle slot*, not per round: a bundle
    // moves only when the balloon holding it wakes to transmit, which is the
    // same slot it beacons on. So one hop costs beacon_interval_rounds, and a
    // path of depth d takes d times that, one way.
    /// How often a balloon originates a telemetry bundle (200 rounds ≈ 6.7 sim
    /// hours). Measured, not chosen: at 25 the mesh gridlocks — every balloon
    /// is permanently carrying, so nobody can relay, and delivery falls to 29%
    /// with 81k blocked handoffs. Backing off to 200 leaves in-flight at ~half
    /// the fleet and raises delivery to 41%. See bin/bundle_delivery.rs.
    ///
    /// **This is the demand knob, and for a long time it was the demand knob
    /// on a saturated resource.** Measured under the old one-bundle-per-contact
    /// rule: only ~23 of 1200 balloons can hear a tower at any moment, giving a
    /// ceiling of ~4.7 deliveries/round, and sweeping this 50 -> 1600 moved
    /// offered load 28x while delivery stayed pinned near 3.1/round —
    /// completion moved 16% -> 93% purely because the denominator changed.
    /// Queues, TTLs and route selection were all measured and none moved it.
    ///
    /// **That ceiling no longer binds at this interval.** With
    /// tower_contact_bundles = 4 the last hop can pass ~16.6/round against the
    /// ~5.9/round this offers, so the demand sweep above would come out
    /// differently today and should be re-run before being quoted. 200 is kept
    /// because it puts the mesh visibly under load without being hopeless.
    /// See docs/investigations/bundle-delivery-report.html §5–§7 and §4.
    pub bundle_interval_rounds: u64,
    /// How long a bundle may go unresolved before it is given up on. Must
    /// exceed a deep-path traversal (14 hops x beacon_interval_rounds = 70
    /// rounds) or bundles expire while legitimately in flight. This is the
    /// satellite-fallback timeout: a bundle that ages out — wherever it
    /// currently sits, not just at its origin — is handed to satellite rather
    /// than dropped. It doubles as the ack timeout: an ack is stamped with its
    /// bundle's `created_at_round`, so a round trip has to complete inside the
    /// same 150-round budget as the one-way bundle timeout, not a separate one
    /// — enough room even at the depth this is sized against (14 hops = 140
    /// rounds round-trip).
    pub bundle_max_age_rounds: u64,
    /// How many bundles a balloon may hold at once — its store-and-forward
    /// buffer.
    ///
    /// Separate from the one-outstanding-bundle rule on *origination*.
    /// Conflating the two was a design error: with a single slot a balloon
    /// holding its own bundle cannot relay anyone else's.
    ///
    /// **What bounds this physically is not memory, and not energy directly.**
    /// A telemetry bundle is a few hundred bytes, so even a small MCU holds
    /// thousands — storage is free. Energy is the real constraint, but it
    /// limits *transmitting*, not *holding*: a LoRa transmit burst draws
    /// ~120 mA while SRAM retention costs microamps. That constraint is
    /// already modelled, as beacon_interval_rounds — the duty cycle — not as
    /// queue depth.
    ///
    /// The bound that does apply is derived from the other two: a balloon
    /// drains one bundle per duty-cycle slot, so anything sitting deeper than
    /// bundle_max_age_rounds / beacon_interval_rounds = 30 positions expires
    /// before its turn ever comes.
    ///
    /// Measured (bin/bundle_delivery.rs, 1200 balloons, degree ~6), after
    /// tower_contact_bundles landed:
    ///
    /// | capacity | 1 | 2 | 4 | 8 | 16 |
    /// |---|---|---|---|---|---|
    /// | blocked % of slots | 78.5 | 44.9 | 27.5 | 7.9 | 2.8 |
    /// | completion | 39.2% | 72.3% | 60.2% | 80.2% | 64.3% |
    ///
    /// **Read the blocked row, not the completion row.** Completion is
    /// non-monotone because it is dominated by how many balloons happened to be
    /// in tower range that run — that count varied 15.0 to 25.8 and correlates
    /// with delivered/round at r = 0.94, which swamps any queue effect at
    /// single-seed. Blocking is the unconfounded signal, and it is monotone.
    pub relay_queue_capacity: usize,
    /// How many bundles ride one transmission, per link kind. The
    /// `tower_contact` half is the measured last-hop lever:
    ///
    /// One transmission per wake slot is the right rule for a *beacon*: it is a
    /// broadcast to no one in particular, rationed by the duty cycle. A tower
    /// contact is a different event — a point-to-point link to a station with
    /// mains power and a real antenna — and spending one wake's whole airtime
    /// on it is both physically reasonable and the only lever that acts
    /// directly on the measured bottleneck.
    ///
    /// Set to 1 this reproduces the old one-bundle-per-slot behaviour exactly,
    /// which is how the two were compared. Measured (1200 balloons, coeff 4.12,
    /// 2000 rounds), completion rate:
    ///
    /// | window | 1 | 2 | 4 | 8 |
    /// |---|---|---|---|---|
    /// | completion | 51.2% | 60.7% | **67.4%** | 66.0% |
    ///
    /// **4, not 8** — the gain saturates because past ~4 the last hop stops
    /// being the binding constraint (ceiling 16.6/round against 5.9/round
    /// offered) and the limit moves back into the mesh, to bundles expiring
    /// before they ever reach a tower-adjacent balloon.
    pub batch: BatchPolicy,
    /// Hop budget. A bundle whose recorded path reaches this length is dropped.
    /// Sized against p95 mesh depth (see mesh_depth.rs); deeper paths only
    /// exist near percolation, where satellite is the right answer anyway.
    pub bundle_max_hops: usize,
    /// How many acks a balloon may hold at once. Much smaller than
    /// relay_queue_capacity: an ack is only ever produced
    /// one-per-successful-delivery, riding a path that *just* worked, so ack
    /// volume is a fraction of bundle volume. A full ack queue drops the
    /// incoming ack — deliberately lossy, the "acks can be lost" property §4
    /// calls out, not a case worth a hold-and-retry rule of its own.
    pub ack_queue_capacity: usize,
    /// Which held bundle moves when a slot comes up.
    pub queue_discipline: QueueDiscipline,
    /// How a delivery is acknowledged back to its origin.
    pub ack_policy: AckPolicy,
    /// Under `AckPolicy::Digest`, how many recent deliveries a tower announces
    /// per beacon, freshest first. Bounds the beacon's size; entries also age
    /// out after `belief_max_age_rounds`, past which the origin has given up
    /// anyway and the announcement would be telling it nothing it can use.
    pub ack_digest_entries: usize,

    // --- Telemetry records (design doc §1) -----------------------------------
    /// How many telemetry records a balloon retains locally. Records are
    /// created 1:1 with bundle origination, so at bundle_interval_rounds = 200
    /// a 720-round sweep produces only ~3-4 per balloon — generous headroom
    /// rather than a binding limit. The bound exists because an unbounded log
    /// is the kind of slow leak that looks fine in a 400-round harness run and
    /// eats memory in a server left up overnight.
    pub comms_log_capacity: usize,
}

impl Default for DvDtnParams {
    fn default() -> Self {
        DvDtnParams {
            beacon_interval_rounds: 5,
            beacon_jitter_rounds: 1,
            belief_max_age_rounds: 60,
            beacon_max_hops: 20,
            metric: Metric::default(),
            discovery: Discovery::default(),
            reply_policy: ReplyPolicy::default(),
            lsa_per_transmission: 4,
            bundle_interval_rounds: 200,
            bundle_max_age_rounds: 150,
            relay_queue_capacity: 8,
            batch: BatchPolicy::default(),
            bundle_max_hops: 20,
            ack_queue_capacity: 4,
            queue_discipline: QueueDiscipline::default(),
            ack_policy: AckPolicy::default(),
            ack_digest_entries: 16,
            comms_log_capacity: 32,
        }
    }
}
