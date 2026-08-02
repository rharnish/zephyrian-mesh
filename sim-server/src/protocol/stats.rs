// A protocol-agnostic way to report counters, so protocols with genuinely
// different vocabularies can still be put in the same table.
//
// The problem this solves showed up the moment a second protocol existed.
// `BundleStats` counts `stall_no_belief`, `stall_stale_next_hop` and
// `ack_lost` — none of which mean anything under a protocol with no beliefs,
// no next hops and no acks. Spray-and-wait instead counts
// `duplicate_arrivals`, which has no counterpart in a unicast protocol. There
// is no useful common struct here, and inventing one would give every protocol
// a pile of fields it must leave at zero.
//
// So each protocol keeps its own concrete stats type — that part was right —
// and additionally publishes a flat keyed table. Consumers that know which
// protocol they are driving keep using the concrete type (see
// `World::bundle_stats`); consumers that don't, read the table.
//
// **Canonical keys.** Cross-protocol comparison only works if the things that
// mean the same thing are named the same thing. Every delivery protocol should
// report at least:
//
//   originated       records a balloon created
//   delivered        records that reached a tower (records, not copies —
//                    replication protocols must deduplicate before counting)
//   resolved         records that left circulation by any route
//   completion_rate  delivered / resolved
//
// Anything beyond that is the protocol's own business, and appears under
// whatever name it likes.

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum StatValue {
    Count(u64),
    Ratio(f64),
}

impl StatValue {
    pub fn as_f64(self) -> f64 {
        match self {
            StatValue::Count(v) => v as f64,
            StatValue::Ratio(v) => v,
        }
    }
}

impl std::fmt::Display for StatValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StatValue::Count(v) => write!(f, "{v}"),
            StatValue::Ratio(v) => write!(f, "{v:.4}"),
        }
    }
}

/// Insertion-ordered, because the order a protocol lists its counters in is
/// how it wants them read — and it makes a printed table stable between runs.
#[derive(Debug, Clone, Default)]
pub struct StatsTable(Vec<(&'static str, StatValue)>);

impl StatsTable {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn count(mut self, key: &'static str, v: u64) -> Self {
        self.0.push((key, StatValue::Count(v)));
        self
    }

    pub fn ratio(mut self, key: &'static str, v: f64) -> Self {
        self.0.push((key, StatValue::Ratio(v)));
        self
    }

    pub fn get(&self, key: &str) -> Option<f64> {
        self.0.iter().find(|(k, _)| *k == key).map(|(_, v)| v.as_f64())
    }

    pub fn keys(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.0.iter().map(|(k, _)| *k)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&'static str, StatValue)> + '_ {
        self.0.iter().copied()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}
