# Aggregation: batching mesh hops, and acks that cost nothing

**Data:** [`aggregation-sweep-results.csv`](aggregation-sweep-results.csv) —
1152 rows: 24 seeds × {600, 1200, 2000} balloons × {1, 4} tower contact ×
{1, 2, 4, 8} mesh batch × {source-routed, digest} acks, 800 rounds each, zero
wind, coeff 4.12. 2.8h on four cores.
**Generator:** [`aggregation_sweep`](../sim-server/src/bin/aggregation_sweep.rs) ·
**Tables:** [`summarize_aggregation.py`](summarize_aggregation.py)

## The question

[`MESH_COMMS_DESIGN.md`](../docs/design/MESH_COMMS_DESIGN.md) §4 closed a long
investigation with the finding that delivery is limited by the **last hop**:
only ~23 of 1200 balloons can hear a tower at any moment. Widening the tower
contact window from 1 to 4 bought +16 points of completion and then saturated,
at which point the limit moved *back into the mesh* — and the doc stopped
there: "the remaining loss is now a different problem from the one investigated
here."

This is that problem. Both levers raise **information per transmission** rather
than transmissions per second, the one direction the earlier work never tried:

- **Batching** (`BatchPolicy`) generalizes the tower-contact window to
  balloon-to-balloon hops, so one wake slot can carry several bundles.
- **The ack digest** (`AckPolicy::Digest`) stops sending receipts as packets.
  Towers announce recent deliveries inside beacons they were already sending,
  and the announcement floods outward with the wave.

Both are transmission-time changes only: a batched hop carries K individually
identified bundles, each with its own origin, seq and path, so per-record
provenance survives and `delivered` still counts records.

## Headline

Completion %, mean ± sd across 24 seeds, at n = 1200, tower_contact = 4:

| acks | mesh=1 | mesh=2 | mesh=4 | mesh=8 |
|---|---|---|---|---|
| source-routed | **62.6 ± 6.8** | 76.8 ± 6.6 | 87.3 ± 4.7 | 90.4 ± 4.0 |
| digest | 73.6 ± 6.3 | 89.5 ± 4.1 | **94.2 ± 2.6** | 94.4 ± 2.5 |

Top-left is what ships. Both levers work; together they roughly halve the
shortfall twice over, 62.6% → 94.2%.

## The two levers are substitutes, not complements

**This corrects the provisional single-seed reading of this file, which had it
backwards.** From one seed it looked as though neither lever was much use
without the other. Paired across 24 seeds, the opposite is true: each is worth
*less* when the other is already in place.

Effect of the digest, as (digest − source-routed) at the same seed, in
completion points — n = 1200, tower_contact = 4:

| mesh=1 | mesh=2 | mesh=4 | mesh=8 |
|---|---|---|---|
| **+11.1 ± 1.8** | +12.6 ± 3.7 | +6.9 ± 4.0 | +4.1 ± 3.1 |

The digest is worth +11 points on its own and only +4 once mesh hops already
carry 8 — by which point the effect is no longer clearly distinguishable from
zero at this sample size. Symmetrically, batching is worth +24.8 ± 3.8 points
under source-routed acks and +20.6 ± 5.7 under the digest.

The reason is that **both levers spend the same currency**. Acks were not
merely occupying airtime, they were *winning* it: `bundle.rs` deliberately
gives a relayed ack priority over that balloon's own forwarding, since letting
both ride one wake would double its per-slot throughput. So the digest frees
wake slots, and batching makes each remaining slot carry more. Do either and
the mesh moves more per unit airtime; do both and the second one finds less
left to recover.

Batching is the larger lever of the two at every density measured.

## The finding that isn't about throughput

Share of *delivered* bundles whose receipt never got home — the band where a
balloon cannot distinguish "never arrived" from "arrived, receipt died":

| acks | mesh=1 | mesh=2 | mesh=4 | mesh=8 |
|---|---|---|---|---|
| source-routed | 35.1 ± 6.1 | 46.5 ± 5.8 | 53.5 ± 5.2 | **55.3 ± 5.1** |
| digest | 0.0 ± 0.0 | 0.0 ± 0.0 | 0.0 ± 0.0 | 0.0 ± 0.0 |

(n = 1200, tower_contact = 4.)

Under source-routed acks, **batching makes the acknowledgement problem
strictly worse**: more bundles land, so more receipts contend for the same
slots, and the unacked share climbs from 35% to 55%. At mesh=8 a clear
majority of successfully delivered telemetry leaves its origin believing it
failed.

The digest removes this by construction — no ack packet exists, so none can be
lost — and the zeros carry no variance because it is not a measured effect but
a structural one.

For the project's central theme this matters more than the throughput
numbers. The belief-vs-truth gap that motivates the whole simulator is usually
shown as stale routes; here it is a balloon that *did* get its data home and
has no way to know. Tuning for throughput alone would have widened that gap
while making the delivery figures look better.

## Density, and where the ground link still binds

| n | best completion (digest, mesh=8), tower=4 | tower=1 |
|---|---|---|
| 600 | 34.7 ± 13.6 | 32.4 ± 12.6 |
| 1200 | 94.4 ± 2.5 | 62.1 ± 8.1 |
| 2000 | 96.2 ± 1.5 | — |

At **n = 600** the mesh is below percolation and nothing helps: every
configuration sits near 30%, and the ±13 spread says the answer is decided by
which balloon field you drew, not by policy. Both levers are irrelevant when
there is no connected path to spend airtime on.

At **tower_contact = 1** the ground link is still the cap, and the levers are
worth much less: the digest gains +5.0 ± 1.3 at n = 1200 rather than +11.1,
and the best configuration reaches 62% against 94%. So the design doc's
last-hop finding survives intact — **the ground link has to stop binding
before mesh airtime becomes worth spending**, and that part of the provisional
reading was right.

Ceiling utilisation at tower_contact = 4 runs 19–31%, so the ground link is
now far from saturated and the mesh is squarely the constraint again.

## On method

Comparisons here are **paired by seed**, and that is doing real work. The
protocol draws from an RNG stream independent of the world's
(`MeshProtocol::reseed`), so every configuration at a given seed runs over an
*identical* balloon field — there is a test asserting exactly that
(`protocol_choice_does_not_perturb_the_balloon_field`). Differences can
therefore be taken per seed before averaging.

The payoff is visible: the digest effect at n = 1200, mesh = 1 is **+11.1 ±
1.8**, while the *levels* it is computed from carry ±6.8 and ±6.3. Comparing
marginal means would have buried an 11-point effect inside a 7-point spread.

The single-seed figures this file previously reported (seed 42) were
systematically optimistic by 3–6 points at every configuration — directionally
right on levels, wrong on the relationship between the levers.

## Coda: how this compares to not routing at all

A different axis, included here because the digest+batch configuration above
is one of its rows. `protocol_compare` runs several protocols over identical
balloon fields (4 seeds, n = 1200, 400 rounds):

| protocol | completion |
|---|---|
| dv-dtn (shipped) | 72.3 ± 7.7% |
| dv-dtn, digest + mesh=4 | **95.0 ± 1.4%** |
| dv-dtn, reactive discovery (AODV-style) | 57.3 ± 5.0% |
| dv-dtn, reactive, tower-only replies | 60.0 ± 5.8% |
| binary spray-and-wait, L=4 | 11.0 ± 1.8% |
| binary spray-and-wait, L=16 | 23.4 ± 4.5% |

Blind replication does badly here, and the protocol-specific counters say why:
`handoffs_per_delivery` runs 40–58, and `no_candidate` — wake slots where a
holder had no neighbour lacking the record — is enormous. A random walk rarely
stumbles onto the ~27 of 1200 balloons that can hear a tower, while dv-dtn
steers at them. Raising the copy budget helps (10.9% → 23.5%) at 3× the
handoffs and 340× the blocking, since copies congest the very queues they need.

**This is the design doc's own premise showing up as a measurement.** §1.1
established that links here are quasi-static — a balloon drifts ~0.4% of link
range per link round — which is precisely the regime where maintaining a route
is cheap and worth it. Replication earns its keep when that is false: when
contacts are brief and a route cannot be kept current long enough to use.

### Reactive discovery, and a bug worth recording

The reactive rows cost 15 points against maintaining routes continuously, and
the counters say the cost is exactly what the mechanism predicts:
`stall_no_belief` is **23,259** against proactive's **1,118** — balloons sitting
on a bundle with nowhere to send it, waiting out a flood in each direction at
one hop per wake slot. Believed depth is *shorter* than proactive (5.63 hops
vs 7.12), because an on-demand route is built fresh rather than inherited.

That is the honest reading only after a fix. Reactive first measured **23.2%**
with a believed depth of **13.6 hops** and 586 loop drops, and the tempting
story was "reactive is simply worse under a duty cycle." It wasn't. A node
answering a request from its own route stamped the reply with the *current
round* rather than the age of the news it held. Under freshness-first adoption
that made a stale twelve-hop route look like this instant's knowledge, so it
beat a genuinely current two-hop reply arriving beside it. Routes inflated
instead of converging. Carrying the age through — the same anti-laundering rule
the proactive side already had — moved it 23.2% → 57.3% and 13.6 → 5.63 hops.
Real AODV prevents this with destination sequence numbers; this is the same
defect those exist to close.

Gating replies to tower-adjacent nodes only (`reply=tower`) was the fix I
expected to need, and it turns out to be worth almost nothing once the ages are
honest: +2.7 points, inside the spread, and it *raises* `stall_no_belief` to
29,482 because every request now has to reach the edge of the mesh. Worth
keeping as a parameter, not as a finding.

The general lesson is the one this simulator keeps producing: **a protocol
comparison measures the implementation, not the family.** A 34-point result was
sitting inside a five-line bug, and it looked like a plausible mechanism the
whole time.

So this is **not** "routing beats replication". It is spray-and-wait run in the
regime it is worst suited to, at parameters that were not tuned (L=16 against a
queue capacity of 8 is self-inflicted congestion). The honest claim is
narrower, and the interesting follow-up is to churn the topology hard enough
that dv-dtn's beliefs cannot keep up — which is where the ordering should
invert, and which this simulator can stage by raising the wind or the
retargeting rate.

## Limitations

- **Zero wind.** Topology is near-frozen. Real ERA5 wind churns links and
  should hurt every row, plausibly not equally.
- **`originate = 200` throughout.** Both levers raise capacity; none of this
  says where the mesh breaks under heavier demand. With completion at 94% and
  ceiling utilisation at 31%, raising demand is the obvious next experiment.
- **Digest truncation untested at scale.** At `digest_entries = 16` and these
  delivery rates the window is generous. A denser, faster-delivering field
  could overflow it, at which point origins would learn late rather than never
  — worth measuring before the parameter is trusted.
- **n = 600 is uninformative** rather than negative: the error bars swamp
  every effect. More seeds would help there specifically.
- **The coda's spray-and-wait rows are untuned**, and 4 seeds rather than 24.
  They establish an ordering in this regime, not a bound on the protocol.
- **The reactive rows are not tuned either.** No route caching between
  requests, no expanding-ring search, no destination sequence numbers (the age
  field stands in for them). Given the laundering bug found above, treat 57.3%
  as a floor for the family rather than a measurement of AODV.

## Reproducing

```bash
cd sim-server
RAYON_NUM_THREADS=3 nohup ./target/release/aggregation_sweep 24 800 \
  > /tmp/aggregation-sweep.log 2>&1 &
python3 experiments/summarize_aggregation.py experiments/aggregation-sweep-results.csv
```

Rows append as they finish and are skipped on restart, so the run can be
interrupted, resumed, or extended with more seeds by re-running with a larger
first argument.
