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

/// How long something took, in comms rounds.
///
/// **Why this exists at all.** Until it did, this simulator measured a
/// delay-tolerant network without measuring delay. Every counter in the project
/// answered "did it arrive?" and none answered "how long did that take?" — which
/// leaves the defining trade of the whole field, delay against delivery, half
/// unobserved. Mechanisms that buy delivery by spending time (batching waits for
/// a fuller transmission; an expanding ring pays a round trip per failed ring)
/// were being scored purely on what they gained.
///
/// **Histogram plus an exact sum, not one or the other.** The buckets are coarse
/// enough that a mean derived from them would carry a systematic half-bucket
/// error, and the numbers here get quoted in write-ups. So the mean comes from
/// `sum` and is exact; the buckets only ever serve percentiles, where their
/// granularity is the honest resolution of the answer anyway.
#[derive(Debug, Clone, Copy)]
pub struct LatencyStats {
    /// Rounds per bucket. Five is one balloon's duty cycle at the default
    /// `beacon_interval_rounds`, so a bucket is "one more wake slot" — the unit
    /// latency actually moves in, since a bundle can only advance when its
    /// holder wakes.
    pub hist: [u64; LATENCY_BUCKETS],
    /// Exact total, so `mean` owes nothing to bucket width.
    pub sum: u64,
    pub count: u64,
}

pub const LATENCY_BUCKETS: usize = 32;
pub const LATENCY_BUCKET_ROUNDS: u64 = 5;

impl Default for LatencyStats {
    fn default() -> Self {
        LatencyStats { hist: [0; LATENCY_BUCKETS], sum: 0, count: 0 }
    }
}

impl LatencyStats {
    /// The top bucket saturates rather than being dropped. At the shipped
    /// settings nothing can reach it — a bundle is given up on at
    /// `bundle_max_age_rounds` (150), and 32 five-round buckets span 160 — but
    /// raising that parameter would push samples into it, and a silently
    /// discarded tail is worse than a visibly clamped one. `mean` stays exact
    /// either way because it reads `sum`.
    pub fn record(&mut self, rounds: u64) {
        let bucket = (rounds / LATENCY_BUCKET_ROUNDS) as usize;
        self.hist[bucket.min(LATENCY_BUCKETS - 1)] += 1;
        self.sum += rounds;
        self.count += 1;
    }

    pub fn mean(&self) -> f64 {
        if self.count == 0 {
            return 0.0;
        }
        self.sum as f64 / self.count as f64
    }

    /// Upper edge of the bucket containing the `p`th percentile, in rounds —
    /// so p95 reads "95% of these finished within N rounds". Resolution is one
    /// bucket, which is the truthful precision for a quantile taken from a
    /// histogram; the exact `mean` is the number to quote when precision
    /// matters.
    pub fn percentile(&self, p: f64) -> f64 {
        if self.count == 0 {
            return 0.0;
        }
        let target = (p * self.count as f64).ceil() as u64;
        let mut seen = 0u64;
        for (i, &c) in self.hist.iter().enumerate() {
            seen += c;
            if seen >= target {
                return ((i as u64 + 1) * LATENCY_BUCKET_ROUNDS) as f64;
            }
        }
        (LATENCY_BUCKETS as u64 * LATENCY_BUCKET_ROUNDS) as f64
    }
}

#[cfg(test)]
mod latency_tests {
    use super::*;

    /// The mean must come from the exact sum, not from bucket midpoints.
    /// Three samples that all land in the same 5-round bucket still have
    /// distinct values, and a bucket-derived mean would report them as equal —
    /// which is precisely the error this struct carries a `sum` to avoid.
    #[test]
    fn the_mean_is_exact_not_bucket_derived() {
        let mut a = LatencyStats::default();
        for v in [11u64, 12, 13] {
            a.record(v); // all in bucket 2 (10..15)
        }
        assert_eq!(a.mean(), 12.0);

        let mut b = LatencyStats::default();
        for v in [10u64, 12, 14] {
            b.record(v); // same bucket, same count, different values
        }
        assert_eq!(b.mean(), 12.0);
        assert_eq!(a.hist, b.hist, "identical buckets...");
        assert_ne!(a.sum, 100, "...but the sum is what the mean reads");
    }

    /// A percentile reports the upper edge of its bucket, so it reads as
    /// "95% finished within N rounds" rather than as a point estimate the
    /// resolution cannot support.
    #[test]
    fn percentiles_report_the_bucket_upper_edge() {
        let mut s = LatencyStats::default();
        for _ in 0..99 {
            s.record(1); // bucket 0 -> upper edge 5
        }
        s.record(100); // bucket 20 -> upper edge 105
        assert_eq!(s.percentile(0.5), 5.0);
        assert_eq!(s.percentile(0.95), 5.0, "the 100th sample is past p95");
        assert_eq!(s.percentile(1.0), 105.0, "but it is the maximum");
    }

    /// Samples past the last bucket are clamped into it rather than dropped,
    /// and the mean stays exact regardless — so raising `bundle_max_age_rounds`
    /// degrades the percentile's resolution without corrupting the mean.
    #[test]
    fn an_overlong_sample_saturates_without_being_lost() {
        let mut s = LatencyStats::default();
        s.record(10_000);
        assert_eq!(s.count, 1);
        assert_eq!(s.sum, 10_000);
        assert_eq!(s.mean(), 10_000.0, "the mean is untouched by clamping");
        assert_eq!(s.hist[LATENCY_BUCKETS - 1], 1, "and it lands in the top bucket");
    }

    #[test]
    fn an_empty_histogram_reports_zero_rather_than_nan() {
        let s = LatencyStats::default();
        assert_eq!(s.mean(), 0.0);
        assert_eq!(s.percentile(0.95), 0.0);
    }
}
