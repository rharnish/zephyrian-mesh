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
only ~22 of 1200 balloons can hear a tower at any moment. Widening the tower
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
| 600 | 34.7 ± 13.6 | 29.3 ± 12.1 |
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

Ceiling utilisation at tower_contact = 4 runs 19–31% at n ≥ 1200, so the
ground link is now far from saturated and the mesh is squarely the constraint
again. At n = 600 it is only 8–11%, which is the same percolation story from
the other side: the mesh cannot even fill the last hop it has.

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
balloon fields — n = 1200, 400 rounds, zero wind. Console capture:
[`measurements/protocol-compare.txt`](measurements/protocol-compare.txt).

Where Coda 2's discovery sweep covers the same variant its figure is quoted
instead, because it has 20 seeds against this harness's 8:

| protocol | completion | seeds |
|---|---|---|
| dv-dtn (shipped) | 68.9 ± 6.7% | 20 |
| dv-dtn, digest + mesh=4 | **94.9 ± 2.0%** | 20 |
| dv-dtn, reactive discovery (AODV-style) | 52.5 ± 5.3% | 20 |
| dv-dtn, gossiped link-state | 47.7 ± 5.4% | 20 |
| dv-dtn, reactive, tower-only replies | 58.4 ± 6.6% | 8 |
| binary spray-and-wait, L=4 | 10.2 ± 1.4% | 8 |
| binary spray-and-wait, L=16 | 21.8 ± 3.3% | 8 |

> **The two harnesses agree exactly.** Restricted to the same 8 seeds,
> `discovery_sweep` reproduces every `protocol_compare` figure above to the
> decimal, standard deviations included — 70.4 ± 8.2 for dv-dtn, 49.9 ± 6.0 for
> link-state, and so on for all eight overlapping variants. The only reason the
> table's first four rows differ from the capture file is the wider seed sample,
> not the harness. `reply=tower` and the two spray-and-wait budgets have no
> 20-seed source, so they stay at 8.

Blind replication does badly here, and the protocol-specific counters say why:
`handoffs_per_delivery` runs 43–63, and `no_candidate` — wake slots where a
holder had no neighbour lacking the record — is enormous (27,452 at L=16). A
random walk rarely stumbles onto the ~22 of 1200 balloons that can hear a
tower, while dv-dtn steers at them. Raising the copy budget helps (10.2% →
21.8%) at 3.2× the handoffs and 292× the blocking, since copies congest the
very queues they need.

**This is the design doc's own premise showing up as a measurement.** §1.1
established that links here are quasi-static — a balloon drifts ~0.4% of link
range per link round — which is precisely the regime where maintaining a route
is cheap and worth it. Replication earns its keep when that is false: when
contacts are brief and a route cannot be kept current long enough to use.

### Does this hold across density?

The table above is one density (n=1200). When this question was first asked it
rested on a 4-seed run reading 72.3 ± 7.7% and 95.0 ± 1.4% — a handful of seeds
at a single balloon count, and this project's own history is full of rankings that moved
once density or seed count changed (§"Density, and where the ground link
still binds" above; the reactive-discovery bug below). So: does digest+mesh4's
lead hold as n changes, or was n=1200 special?

**Data:** [`density-sweep-results.csv`](density-sweep-results.csv) — 3,465
rows: all 11 `density_sweep` protocol variants × {100, 200, 400, 600, 800, 1200,
1600, 2000, 3000} balloons × 35 seeds, 400 rounds, zero wind. ~3h47m on four
cores. [`ground-truth-sweep-results.csv`](ground-truth-sweep-results.csv) —
315 rows, the same n × seed grid but protocol-independent (`grounded_pct` is
computed from union-find over balloon positions before any protocol is
consulted, so it only needs one run per (n, seed), not per protocol). ~19.5
min on four cores.
**Generators:** [`density_sweep.rs`](../sim-server/src/bin/density_sweep.rs) ·
[`ground_truth_sweep.rs`](../sim-server/src/bin/ground_truth_sweep.rs)
**Chart:** [`protocol-results/density-sweep.png`](protocol-results/density-sweep.png) ·
[`plot_density_sweep.py`](plot_density_sweep.py). Same chart, interactive
(hover for every protocol's value at a given n, click a legend entry to isolate
one of the eleven): [`protocol-results/density-sweep.html`](protocol-results/density-sweep.html) ·
[`plot_density_sweep_html.py`](plot_density_sweep_html.py). GitHub shows `.html`
files in a repo as source, so the HTML needs Pages or a local open; the PNG is
what embeds in Markdown, and stays the canonical figure.

| protocol | n=1200, 4 seeds (original, sd) | n=1200, 35 seeds (SEM) |
|---|---|---|
| dv-dtn (shipped) | 72.3 ± 7.7% | 69.1 ± 1.0% |
| dv-dtn, digest + mesh=4 | 95.0 ± 1.4% | 95.3 ± 0.3% |

Both land within the original's much wider interval — the 4-seed table's
ranking wasn't a fluke — and the intervals tighten 4.7–7.7×, enough to
separate protocols whose n=1200 intervals used to touch. Note the two columns
are not the same statistic: the 4-seed figures are standard deviations, the
35-seed ones standard errors (6.2/√35 = 1.05), so the comparison is of
*reported interval width*, not of spread.

**The ranking holds, and digest+mesh4's lead widens rather than shrinks as
density rises.** It tracks shipped dv-dtn closely below n=600, then pulls
ahead through the percolation transition and keeps extending its lead out to
n=3000 (96.5%, vs. dv-dtn's own best of 83.9%). Spray-and-wait never
recovers either — L=16 caps at 23.8% completion even at 3000 balloons, so its
n=1200 loss wasn't an under-provisioning artifact.

**Below ~600 balloons, protocol choice barely matters.** At n = 400 every
routing variant — proactive, reactive, link-state — sits within about two
points of every other one (14.1–16.7% completion), and the two spray-and-wait
configurations trail below at 6.9% and 9.6%, for a total spread of under ten
points where the percolated regime spreads more than sixty. The network isn't
percolated yet: there is rarely a route to route well, so nothing a protocol
does can show up as a difference. The interesting separation starts only
once the physical topology has enough paths for routing quality to matter.

**Ground truth explains where the remaining gap goes.** `grounded_pct` — the
share of balloons whose physical component contains a tower, computed before
`self.protocol` is ever consulted — jumps from 29.9% at n=600 to 98.2% at
n=1200 and is indistinguishable from 100% by n=2000. Every protocol keeps
climbing well past that point, which means the gap between a protocol's
completion rate and 100% at high n is now **entirely routing overhead**, not
missing physical paths: at n=3000, with the network fully percolated,
digest+mesh4 still leaves ~3.5 points on the table and shipped dv-dtn leaves
~16. digest+mesh4 is the only variant whose curve visibly hugs the ground-truth
curve through the percolation transition (n=600-1200); everything else peels
off earlier and never closes the gap.

**One tuning conclusion from Coda 2 turns out to be density-dependent.**
Reply overhearing (`reactive+overhear`) was a wash against plain reactive at
n=1200 in both this table (52.3 ± 0.9% vs. 52.8 ± 0.8%) and in Coda 2's own
20-seed study at the same density. At n=3000 it isn't: 67.0 ± 0.4% vs. plain
reactive's 55.2 ± 0.3%, a gap far outside either error bar, and by then
overhearing beats every other reactive or link-state variant including
`reply=tower` (57.7%) — the tweak that *did* help at n=1200. A single-density
tuning sweep could not have found this: the benefit of overhearing scales
with how many neighbours there are to overhear, so it stays invisible until
density is high enough to matter, which n=1200 wasn't.

### Reactive discovery, and a bug worth recording

The reactive rows cost 15 points against maintaining routes continuously, and
the counters say the cost is exactly what the mechanism predicts:
`stall_no_belief` is **24,787** against proactive's **991** — balloons sitting
on a bundle with nowhere to send it, waiting out a flood in each direction at
one hop per wake slot. Believed depth is *shorter* than proactive (5.91 hops
vs 7.57), because an on-demand route is built fresh rather than inherited.
(All four from `discovery-sweep-results.csv`, 20 seeds.)

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
defect those exist to close. (Those are the before/after of that single fix, on
the run that caught it. On the current 20-seed sweep reactive reads 52.5% at
5.91 hops; the size of the bug, not the exact landing point, is the finding.)

Gating replies to tower-adjacent nodes only (`reply=tower`) was the fix I
expected to need, and it turns out to be worth little once the ages are honest:
+3.8 points, within about half the ±6.5 spread on either row, and it *raises*
`stall_no_belief` from 24,088 to 29,990 because every request now has to reach
the edge of the mesh. Worth keeping as a parameter, not as a finding. (From
[`measurements/protocol-compare.txt`](measurements/protocol-compare.txt) at 8
seeds — `reply=` is the one dial the discovery sweep does not cover.)

The general lesson is the one this simulator keeps producing: **a protocol
comparison measures the implementation, not the family.** A 34-point result was
sitting inside a five-line bug, and it looked like a plausible mechanism the
whole time.

### Link-state: excellent routes, hardly any of them

The gossip variant is the most interesting row in the table, because its
failure is entirely on one side of the ledger. Balloons exchange *observations*
— who they can hear — and each computes its own route by searching the map it
assembles. Against proactive dv-dtn:

| | link-state | proactive |
|---|---|---|
| believed depth | **3.19 hops** | 7.57 |
| `dropped_loop` | **1.1** | 82.8 |
| `stall_stale_next_hop` | **never** † | 68.4 |
| `stall_no_belief` | **36,449** | 991 |
| completion | 47.7% | 68.9% |

All rows but one from `discovery-sweep-results.csv` at 20 seeds. † link-state
does not report `stall_stale_next_hop` at all — it recomputes from its own map
rather than following an inherited pointer, so the counter has nothing to count;
proactive's 68.4 is from
[`measurements/protocol-compare.txt`](measurements/protocol-compare.txt), the
one run where both are printed side by side.

Every quality measure is better and by a wide margin — the routes it finds are
less than half as long, it never forwards to a dead next hop, and it essentially
cannot form a loop, all of which is exactly what computing a path from a map
should buy. It loses anyway, because **most balloons have no route at all**.

The mechanism is gossip bandwidth, and the `lsa` dial confirms it directly.
`lsa` is how many observations ride one transmission — the same
information-per-transmission lever as `mesh`, applied to discovery instead of
to payload:

All four rows are from `discovery-sweep-results.csv` at 20 seeds, n = 1200, 400
rounds, after the determinism fix:

| lsa | completion | believed depth | `stall_no_belief` |
|---|---|---|---|
| 2 | 36.6 ± 4.5% | 2.69 | 42,582 |
| 4 (default) | 47.7 ± 5.4% | 3.19 | 36,449 |
| 16 | 61.7 ± 6.1% | 4.27 | 23,694 |
| 64 | 64.6 ± 6.8% | 5.70 | 9,764 |

Monotone in both directions at once: more gossip per slot means fewer balloons
stranded without a map, *and* longer routes, because a fuller map can see paths
that a partial one simply did not contain.

**But it does not converge on proactive, and the ladder flattens before it
could.** Proactive reads 68.9% on the same sweep; `lsa = 64` reaches 64.6% and
is still 4.3 points short — having bought only 2.9 points over `lsa = 16` for
four times the records per slot. An earlier, pre-determinism-fix run of this
table put `lsa = 64` at 71.6% and had it overtaking proactive; that figure does
not survive re-measurement, and the "catches up given enough airtime" reading
that went with it does not either. What the airtime demonstrably buys is
knowledge, not delivery: `stall_no_belief` falls 4.4× across the ladder while
completion gains 28 points and then stalls.

**This is the classical objection to link-state routing, arrived at from the
other end.** The textbook version is an asymptotic argument about flooding
overhead scaling with network size. Here it is a duty-cycled radio with a finite
slot, and the constraint bites at 1200 nodes: the map cannot be kept current
across the fleet from the airtime available, so what a balloon holds is a small,
accurate, local picture that usually contains no tower. Distance-vector wins
this regime not by being smarter but by being *cheap enough to run everywhere* —
a hop count is a summary, and summarising is what fits in the slot.

Worth noting for the project's central theme: link-state's `stall_no_belief` is
an *honest* stall. A balloon whose map shows no path knows there is no path,
which is a strictly stronger statement than the distance-vector variants can
make — there, no belief is indistinguishable from having heard nothing lately.
Link-state trades delivery for self-knowledge. The simulator does not currently
score that trade, and arguably should.

So this is **not** "routing beats replication". It is spray-and-wait run in the
regime it is worst suited to, at parameters that were not tuned (L=16 against a
queue capacity of 8 is self-inflicted congestion). The honest claim is
narrower, and the obvious follow-up was to churn the topology until dv-dtn's
beliefs could not keep up, which is where the ordering should invert.

### That follow-up ran across seven weather fields, and the ordering did not invert

**Data:** [`wind-sweep-results.csv`](wind-sweep-results.csv) — 960 rows: 8 wind
conditions (zero, plus seven hourly ERA5 fields from 1978-06-09) × 20 seeds × 6
protocols, n = 1200, 400 rounds. ~3h on two cores.
**Generator:** [`wind_sweep`](../sim-server/src/bin/wind_sweep.rs) ·
**Tables:** [`summarize_wind.py`](summarize_wind.py)

Completion %, mean ± sd across 20 seeds:

| protocol | zero wind | real wind (7 fields pooled) | difference |
|---|---|---|---|
| dv-dtn (shipped) | 68.9 ± 6.7 | 70.2 ± 6.1 | +1.3 ± 1.6 |
| dv-dtn digest + mesh=4 | **94.9 ± 2.0** | 92.8 ± 2.8 | **−2.0 ± 0.5** |
| dv-dtn reactive | 52.6 ± 5.3 | 50.2 ± 5.4 | −2.4 ± 1.3 |
| dv-dtn link-state | 47.7 ± 5.4 | 48.0 ± 4.7 | +0.4 ± 1.3 |
| spray-and-wait L=4 | 9.4 ± 1.7 | 10.2 ± 1.6 | +0.8 ± 0.4 |
| spray-and-wait L=16 | 20.4 ± 3.1 | 21.8 ± 3.2 | +1.4 ± 0.7 |

The link-state row was regenerated after the link-detection determinism fix;
the other five predate it and did not need to be, having been verified
bit-identical across the fix. At zero wind the regenerated column now matches
`discovery-sweep-results.csv` on all 20 seeds exactly, so the two CSVs agree.

**Compared within every (wind, seed) cell, dv-dtn beats spray-and-wait L=16 in
160 of 160 cells.** The narrowest margin anywhere is **+38.0 points**. This is
not a close call that weather might tip.

**The churn is real and it reached the protocol.**
(Console capture: [`measurements/link-churn-zero.txt`](measurements/link-churn-zero.txt)
and [`measurements/link-churn-wind.txt`](measurements/link-churn-wind.txt).)
`link_churn` measures link
turnover rising 4.1× (0.091% → 0.376% per round, half-life 757 → 184 rounds) at
unchanged density, and dv-dtn's own counter agrees: `stall_stale_next_hop` goes
42 → ~197, a 4.7× rise, consistent across all seven fields. Mean degree is
effectively unchanged (6.22 → 6.23). Routes go stale nearly five times as often
and delivery does not care.

**Store-carry-forward absorbs it.** A balloon whose next hop has gone keeps
carrying the bundle and forwards it later by another route. Churn becomes delay
rather than loss.

### Weather matters ~10× less than which balloon field you drew

The reason the single-field result was safe, quantified — sd of the per-weather
means against the typical sd across seeds *within* one weather:

| protocol | sd across weather fields | sd across seeds |
|---|---|---|
| dv-dtn | 0.58 | 6.22 |
| dv-dtn digest + mesh=4 | 0.41 | 2.74 |
| dv-dtn reactive | 0.49 | 5.54 |
| spray-and-wait L=16 | 0.15 | 3.29 |

An order of magnitude apart at every row. Which hour of weather you simulate is
nearly irrelevant next to which balloon field you happened to draw — so the
earlier one-field result was not luck, and adding more weather would not change
it. This is the measurement that turns "wind changed nothing that afternoon"
into a statement about wind.

### The one real effect: batching is what churn can hurt

Only one row moves outside two standard errors, and it is the *best* one:
digest + mesh=4 loses **2.0 ± 0.5** points. The counters say why —
`dropped_ttl` rises 11.4 → 19.4 (1.7×), while plain dv-dtn's barely moves
(1.2 → 1.8).

So churn does convert into delay, exactly as claimed above — but **delay is
only free when there is slack to absorb it.** At 95% completion the mesh is
running near its ceiling, queues drain instead of backing up, and a bundle that
loses its route rides until its TTL expires rather than waiting in a queue that
was going to be slow anyway. The configuration with the most throughput has the
least room to absorb disruption, and is the only one where churn costs
deliveries.

That is a refinement of the store-carry-forward argument rather than a
counterexample to it, and it is a caution worth carrying into any tuning work:
**the gains from aggregation are slightly softer under real weather than the
frozen-topology tables suggest**, and 4 seeds on one field could not resolve it
(the same comparison there read −2.0 ± 1.9, a hair over one standard error).

Note also that both replication rows gain slightly under wind (+0.8, +1.4).
That is the mechanism replication exists for — motion carrying copies within
earshot of towers they could not otherwise reach — showing up with the right
sign and an entirely irrelevant magnitude against a 48-point deficit.

This also corrects a prediction made from these same measurements. Reading the
churn figures, I first estimated path survival — a 7-hop path is in transit ~35
rounds and all 7 links must hold — and got 80% → 40%, concluding delivery
should roughly halve. It didn't move at all, because that model assumes a
broken path means a lost bundle. **That is precisely the assumption
store-carry-forward exists to violate.** Path survival is the right model for
an end-to-end circuit; it is the wrong model for DTN.

Which sharpens the coda's original point rather than overturning it.
Replication cannot close the gap by being robust to churn, because the routed
protocol here *is already* robust to churn — carrying a bundle is redundancy in
time, and it substitutes for spray-and-wait's redundancy in space. The two
approaches are not competing on this axis at all.

What would still invert the ordering is churn fast enough that a bundle cannot
be carried to a tower within its TTL at all. Real weather at these altitudes
does not supply that: 4× turnover leaves link half-life at 184 rounds against a
150-round bundle lifetime. It would take a synthetic mechanism, and the result
would be about the mechanism rather than about the atmosphere.

## Coda 2: tuning the two losing families

The reactive and link-state rows above were untuned, and the limitations section
below named the three obvious mechanisms — OLSR's multipoint relays, AODV's
expanding-ring search, and learning from replies not addressed to you. All three
are now implemented and measured.

**Data:** [`discovery-sweep-results.csv`](discovery-sweep-results.csv) — 320
rows: 16 variants × 20 seeds, n = 1200, 400 rounds, zero wind. **Generator:**
[`discovery_sweep`](../sim-server/src/bin/discovery_sweep.rs) · **Tables:**
[`summarize_discovery.py`](summarize_discovery.py)

Contrasts are paired within seed, which is not optional here: between-seed
spread is 5-7 completion points and the effects are 0.3-4, so unpaired means
cannot tell a small real effect from noise.

### Two delivery ratios, and why they disagree

| | completion | delivered/originated |
|---|---|---|
| definition | delivered / **resolved** | delivered / **originated** |
| ignores | bundles still stranded at the end of the run | nothing |

**`completion_rate` flatters a protocol that strands bundles**, because a bundle
still sitting in a queue at round 400 never resolves and so leaves the
denominator. Reactive discovery strands a great many. The two ratios rank the
overhearing result oppositely, so both are reported throughout; picking one
would have been a choice rather than a measurement.

### Reply overhearing — `discovery=reactive,overhear=on`

A route reply is a radio transmission, and route *requests* in the same file
already reach every neighbour of the sender. Restricting a reply to its
addressee was modelling a wire, not a radio. Letting every neighbour in earshot
install the route costs **no additional transmissions at all**.

| | Δ completion | Δ delivered/orig |
|---|---|---|
| overhear vs reactive | −0.29 ± 0.42 | **+3.91 ± 0.39** |

The mechanism does exactly what it was built to do, and the counters are
unambiguous:

| | reactive | +overhear |
|---|---|---|
| `stall_no_belief` | 24787 | **8791** (−65%) |
| `satellite` | 692 | **241** (−65%) |
| `dropped_loop` | 131 | **679** (5.2×) |

Balloons stop sitting on bundles with nowhere to send them, and satellite
rescues fall by two thirds because bundles now have routes instead of timing
out. What it gives back is loops. **Opportunistic adoption does not produce a
globally consistent route set**: an overhearer installs a route computed for
somebody else, and the transmitter's own path may run back through the
overhearer.

Gating adoption on improvement — an overhearer takes a route only if strictly
shorter, never on freshness alone, since it has no standing to treat another
node's answer as an answer to its own question — was tried and moved loop drops
barely at all (610 → 638 at 4 seeds). The remaining loops need real destination
sequence numbers, which is what actual AODV uses and what this simulator's
`emitted_at_round` only approximates.

So: **a genuine +3.9 points of absolute delivery, a wash on completion, and the
honest reading is that the stalling problem is solved and replaced by a smaller
routing-consistency problem.**

### Expanding-ring search — `discovery=reactive,ring=expanding`

| | Δ completion | Δ delivered/orig |
|---|---|---|
| ring vs reactive | **−2.01 ± 0.37** | **−2.15 ± 0.29** |

A clean negative, and the mechanism worked — that is what makes it interesting
rather than a bug. Routes get shorter exactly as the textbook says (believed
depth 5.91 → 4.51) and loop drops nearly vanish (131 → 28), because a ring finds
the *nearest* answer instead of whichever answer shouts back first.

It loses on latency, and the counter says so directly: `stall_no_belief` goes
**24787 → 31104**. Under a duty cycle a failed ring costs a full round trip at
one hop per wake slot, so the search spends more time waiting than the shorter
routes save. The textbook motivation for expanding ring is *airtime*, and
airtime spent on rebroadcasts is not what binds here.

Combining both is worse than overhearing alone (−4.95 ± 0.35 on
delivered/orig), and worse than plain reactive too (−1.04 ± 0.27): the ring
delays the very replies overhearing wants to spread. Against the ring alone it
is a modest gain (+1.11 ± 0.27), so overhearing still helps — just far less
than it does without a ring in front of it.

### MPR relay selection — `discovery=link-state,relay=mpr`

Each balloon names the smallest subset of its neighbours that still reaches
everything two hops out, and only those rebroadcast for it. Two-hop knowledge is
free here, because neighbour lists are the only thing this variant ever sends.

The mechanism works, and cleanly:

| `lsa` | redundant share, flood → MPR | share of neighbours relaying |
|---|---|---|
| 2 | 0.31 → **0.20** | 0.48 |
| 4 | 0.52 → **0.36** | 0.48 |
| 16 | 0.71 → **0.52** | 0.48 |

Total record-receptions fall ~25% while *useful* records rise slightly, and
coverage is provably preserved — a property pinned by a randomised test over 200
neighbourhood shapes, not just the hand-built case.

| contrast | Δ completion | Δ delivered/orig |
|---|---|---|
| MPR at `lsa=2` | +0.28 ± 0.22 | +0.22 ± 0.18 |
| MPR at `lsa=4` | +0.69 ± 0.16 | +0.54 ± 0.14 |
| MPR at `lsa=16` | +0.66 ± 0.13 | +0.46 ± 0.11 |

Real, consistent, and **about a tenth of what was predicted.** The prediction,
made in this repo from the redundancy measurement, was +8-10 points at `lsa=4`.

**The prediction was wrong for a reason worth recording, because it is about the
model rather than about OLSR.** The argument was that MPR "does not ask for more
airtime, it stops spending existing airtime on records the receiver already
holds." That is true of a real radio and false of this simulator: gossip and
bundle forwarding happen on the *same* wake slot here, not competing ones, so
freeing gossip capacity frees nothing that bundle delivery can spend. MPR saves
**bytes**, and this model charges for **slots**.

The residual +0.6 is not nothing — it comes from `lsa`-sized batches carrying
more distinct records — but the mechanism's main benefit is invisible to the
delivery metric by construction. Charging per record rather than per slot is the
change that would make this measurable, and it would alter far more than this
one result.

### A reproducibility bug found while parallelising the sweep

Parallelising `discovery_sweep` across cores turned up something worth more than
the speedup. Checking that one thread and four threads agreed, they did not —
but only on link-state rows, and with `gossip_redundant_share` and `mpr_share`
byte-identical. Running single-threaded three times settled it: **link-state
had never been reproducible run to run**, same seed, same thread count.

The cause was in shared code, not link-state.
[`link_detection.rs`](../sim-server/src/link_detection.rs) assembled its edge
list with `edges_by_pair.into_values().collect()`, and `HashMap` iteration order
is randomised per map. That order becomes each balloon's neighbour-list order in
`MeshAdjacency::rebuild`.

Why only link-state noticed:

| | how it uses neighbour order | affected |
|---|---|---|
| proactive | compares epoch and hop count; same answer whatever order offers arrive in | no |
| reactive | same adoption rule | no |
| **link-state** | BFS returns the **first** tower found at minimum depth — equal-cost routes are chosen between positionally | **yes** |

A probe comparing two identical runs in one process found them diverging by
round 3: same tower, same hop count, different `next_hop`.

Sorting the edge list fixes it, and the fix is verifiable from three directions:
every variant now reproduces in-process; one thread, four threads and a re-run
are byte-identical; and the **golden fingerprint is unchanged**, which
independently confirms proactive really was order-insensitive.

**What it changes in the numbers.** Every link-state figure previously published
carried run-to-run spread that was not seed variance — it inflated link-state
error bars and the noise floor of any contrast built on them. Re-measured
against the clean build, the reactive contrasts are *byte-identical* to the
noisy run (the bug never touched them), while the MPR standard errors fall
20-28% and the point estimates barely move:

| | noisy | clean |
|---|---|---|
| MPR at `lsa=4` | +0.64 ± 0.20 | +0.69 ± 0.16 |
| MPR at `lsa=16` | +0.87 ± 0.18 | +0.66 ± 0.13 |

Link-state *levels* moved more than its contrasts did — `lsa=16` reads 61.7%
clean against 64.1% noisy — because a canonical tie-break is a fixed arbitrary
choice where a random one effectively sampled. Neither is more correct as
routing; the sorted one is reproducible, which is the property every experiment
in this directory depends on.

### What this does and does not change

Nothing here reorders the families. Proactive dv-dtn still leads every routing
alternative, and digest+batch still leads everything. What the tuning establishes
is that **the reactive and link-state deficits are not artefacts of a lazy
implementation**: the three standard mechanisms that ought to close them were
implemented faithfully, two of them worked exactly as specified at the mechanism
level, and the family ordering did not move.

The reactive baseline is unchanged by this work: a hop-limit off-by-one
corrected along the way (a request with `ttl = k` now travels `k` hops rather
than `k+1`) moves it **−0.07 ± 0.20 points**, and proactive reproduces its
published figure to +0.001.

Link-state levels *are* affected, by the determinism fix rather than by MPR —
`dv-dtn:discovery=link-state` reads **47.7 ± 5.4**, against 48.2 ± 5.4 as the
wind sweep originally published it. That gap was the wind sweep's CSV being
older than the fix, not a disagreement between experiments: its link-state
column has since been regenerated and now matches this one on all 20 seeds
exactly. The `lsa` ladder re-measured at 20 seeds reads **2 → 36.6%, 4 → 47.7%,
16 → 61.7%, 64 → 64.6%**, below the earlier small-sample figures at every rung.
Those earlier numbers were taken under randomised neighbour order and are not
reproducible; these are.

`lsa = 64` had never actually been swept — it was quoted from a single
pre-fix run at 71.6%, which had link-state overtaking proactive. Measured
properly it reaches 64.6% and does not overtake. That is the one conclusion in
this file the re-measurement reverses rather than merely tightens.

## Coda 3: latency, the axis that was missing

Until now this simulator measured a **delay**-tolerant network without measuring
delay. Every counter answered "did it arrive?" and none answered "how long did
that take?" — leaving the defining trade of the field half observed, and letting
mechanisms that buy delivery by spending time be scored purely on what they
gained.

Three clocks now run, all in comms rounds, where **5 rounds = one wake slot**:

| clock | from | to | isolates |
|---|---|---|---|
| `first_hop_latency` | origination | leaving the origin at all | **discovery** wait |
| `delivery_latency` | origination | arrival at a tower | the trip |
| `ack_latency` | origination | the *origin finding out* | the full round trip |

The third is the one a balloon actually experiences. Until it fires, the balloon
believes nothing has happened.

### The prediction this refutes

`NEXT-STEPS` recorded the expectation that **batching buys delivery with hidden
latency** — "`mesh=8` waits for a fuller batch, so its +24.8 delivery points may
be bought with latency nothing currently sees." Measured, paired over 20 seeds:

| | Δ delivery latency | Δ ack latency |
|---|---|---|
| `mesh=4` | **−6.8 ± 1.1** | −4.1 ± 0.6 |
| `mesh=8` | **−7.6 ± 1.2** | −4.3 ± 0.6 |
| `ack=digest` | −7.5 ± 0.6 | −6.0 ± 0.6 |
| `digest + mesh=4` | **−20.1 ± 1.2** | −11.2 ± 1.1 |

Batching does not cost latency. **It saves it, substantially**, and p95 delivery
falls 138 → 79 rounds for digest+batch.

The prediction was wrong because it assumed a mechanism the code does not have.
Batching here is *opportunistic*, not accumulating: a wake slot carries up to `K`
bundles **that are already in the queue** (`while ids.len() < batch.mesh_hop`
peeks at what is present and stops). Nothing ever waits for a batch to fill.
Classical Nagle-style batching trades latency for efficiency; this trades
nothing, because the queue is already full of waiting bundles — the constraint
was never "not enough to send", it was "not enough slots to send it in".

Which sharpens the document's main finding rather than complicating it. Both
aggregation levers raise **information per wake slot**, and doing so shortens
every queue in the mesh, so bundles wait behind fewer other bundles. Delivery and
delay improve together because they were both symptoms of the same scarcity.

### Reactive discovery: the cost, finally measured directly

`stall_no_belief` was always the *symptom*. `first_hop_latency` is the thing
itself — how long a bundle sits at its origin before it can move at all:

| variant | first hop | delivery |
|---|---|---|
| proactive | **7.9** | 52.6 |
| reactive | **29.9** | 53.9 |
| reactive + overhear | **15.1** | 49.4 |
| reactive + expanding ring | **40.7** | 57.1 |

Reactive discovery costs **3.8× the time to get moving**. Reply overhearing
halves that (29.9 → 15.1), which is the crispest statement yet of what it does:
it does not improve routes, it removes waiting.

And the expanding ring's negative delivery result is confirmed as a latency
story rather than inferred from a proxy: **+10.8 rounds before a bundle can
move**, because each unanswered ring costs a full round trip at one hop per wake
slot.

### The trap: delivery latency is conditioned on delivery

Read the level table naively and link-state is the fastest protocol in the
project — 26 rounds against proactive's 53. It is not. **It is only timing the
bundles it managed to deliver**, and it delivers the easy ones:

| | delivered/orig | delivery latency | believed depth |
|---|---|---|---|
| `lsa=2` | 25.8% | **26.0** | 2.69 |
| `lsa=4` | 34.9% | 28.9 | 3.19 |
| `lsa=16` | 45.6% | **44.2** | 4.27 |
| `lsa=64` | 47.5% | **50.7** | 5.70 |

**Latency rises monotonically as the protocol gets better.** Widening `lsa`
lets balloons see further, so deeper bundles start arriving — and deeper bundles
are slower. The fast figure at `lsa=2` is survivorship: only balloons within two
or three hops of a tower ever get a route, and of course those are quick.

So `delivery_latency` **cannot be compared across protocols with different
delivery rates.** It is the same class of error as `completion_rate` flattering
protocols that strand bundles (Coda 2), and it is worth stating as a rule:

> Any metric conditioned on success is only comparable between configurations
> that succeed at the same rate. Compare it *within* a protocol across a
> parameter, or pair it with the delivery rate it is conditioned on.

The aggregation contrasts above are safe on this point, and it is worth saying
why rather than asserting it: digest+batch **raises** delivery from 52% to 85%
while **lowering** latency 20 rounds. The selection effect pushes in the opposite
direction to the measured result — a harder set of bundles arriving faster — so
it cannot be manufacturing that finding.

## Limitations

- **Zero wind for the main tables.** The protocol comparison has since been
  repeated on a real ERA5 field (see the coda) and nothing moved, but the
  aggregation sweep itself has not been re-run under wind. Given the digest and
  batching levers act on airtime rather than on route validity, there is no
  particular reason to expect wind to change them — but that is an argument,
  not a measurement.
- **One day of weather.** The wind sweep uses seven hourly fields from a single
  date (1978-06-09), so it samples the diurnal cycle but not the seasonal one.
  Between-weather variance is ~10x smaller than between-seed variance across
  those seven, which is strong evidence the result does not hinge on the hour —
  but a winter jet is a different regime and is not represented. The cache keys
  on the source file's bytes, so adding one is a download rather than a code
  change.
- **Wind conditions are unpaired with each other.** Balloons advect differently
  once wind is applied, so "zero wind, seed 3" and "06:00, seed 3" are different
  fields; only the within-cell protocol comparisons are paired. Differences
  across wind therefore carry the full spread, which is why the −2.0 point
  batching effect needed 20 seeds to resolve.
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
- **The reactive rows are still missing destination sequence numbers.** Coda 2
  added expanding-ring search and reply overhearing; the age field still stands
  in for dest-seq, and the loop drops overhearing introduces are exactly what
  dest-seq exists to prevent. That is the remaining gap between this and AODV
  proper, and it is now the *only* named one.
- **Link-state topology reduction is partly addressed.** Coda 2 added MPR relay
  selection, which cuts redundant gossip by a third at every `lsa`. The other
  two standard reductions — hierarchical areas, and incremental updates instead
  of whole neighbour lists — are not implemented. Incremental updates are the
  more promising of the two here, since they attack payload size, and payload
  size is what MPR turned out not to be charged for.
- **Latency is measured only for bundles that arrive.** See Coda 3 — the figure
  is conditioned on success, so it is not comparable between configurations with
  different delivery rates. There is deliberately no "latency" for a bundle that
  went to satellite (that is just the timeout, 150 by construction) or for one
  still in flight at the cutoff.
- **Airtime is charged per slot, not per byte.** This is the model choice that
  made MPR's measured gain ten times smaller than predicted (see Coda 2), and it
  applies to every result in this file: any mechanism whose benefit is "fewer
  bytes on the air" is invisible here, while any mechanism whose benefit is
  "more done per wake" is fully counted. That asymmetry is why batching and the
  ack digest dominate this document.

## Reproducing

```bash
cd sim-server
RAYON_NUM_THREADS=3 nohup ./target/release/aggregation_sweep 24 800 \
  > /tmp/aggregation-sweep.log 2>&1 &
python3 experiments/summarize_aggregation.py experiments/aggregation-sweep-results.csv
```

For the coda's cross-protocol and wind rows — the wind cache has to be
populated once, with `weather-data-server` running; after that it is read from
disk and the Python side can be stopped:

```bash
cargo run --release --bin wind_cache -- steps          # what the .nc holds
cargo run --release --bin wind_cache -- fetch 0 4 8 12 16 20   # cache a spread
cargo run --release --bin wind_cache -- list

./target/release/link_churn 1978-06-09T04:00:00 1200 400   # is there churn?
RAYON_NUM_THREADS=2 nohup ./target/release/wind_sweep 20 400 &
python3 experiments/summarize_wind.py experiments/wind-sweep-results.csv
```

`link_churn` and `protocol_compare` print a table and write no CSV, so their
runs are captured as console output under
[`measurements/`](measurements/) rather than as a results file — see
[`measurements/README.md`](measurements/README.md) for the exact commands and
what each capture backs.

`wind_sweep` resumes like `aggregation_sweep`: finished rows are appended
immediately and skipped on restart, so it can be interrupted or extended with
more seeds by re-running with a larger first argument.

`--wind none` is the default and means zero wind, so every command above
reproduces the frozen-topology tables unchanged when the flag is omitted.

For Coda 2 (the discovery-mechanism tuning), one command and no wind backend —
it runs on zero wind throughout, ~45 min on one core:

```bash
./target/release/discovery_sweep 20 1200 400 > experiments/discovery-sweep-results.csv
python3 experiments/summarize_discovery.py experiments/discovery-sweep-results.csv
```

Unlike the sweeps above this one does **not** resume: it writes the whole CSV in
one pass, so an interrupted run is restarted rather than continued. At 45
minutes that was not worth the bookkeeping.

For the density-sweep coda, two binaries, neither wind-backed (zero wind
throughout) and neither resumable — both write their whole CSV in one pass,
same tradeoff as `discovery_sweep`:

```bash
./target/release/density_sweep 35 400 > experiments/density-sweep-results.csv       # ~3h47m, 4 cores
./target/release/ground_truth_sweep 35 400 > experiments/ground-truth-sweep-results.csv  # ~20 min, 4 cores
python3 experiments/plot_density_sweep.py experiments/density-sweep-results.csv \
  --out experiments/protocol-results/density-sweep.png \
  --truth experiments/ground-truth-sweep-results.csv
python3 experiments/plot_density_sweep_html.py experiments/density-sweep-results.csv \
  --out experiments/protocol-results/density-sweep.html \
  --truth experiments/ground-truth-sweep-results.csv
```

`density_sweep`'s job count is protocols × n-grid × seeds; the first argument
is seeds, so halving it roughly halves the runtime at the cost of wider error
bars. `ground_truth_sweep` shares the same n-grid and seed count but drops the
protocol dimension, since `grounded_pct` is computed before any protocol is
consulted and is therefore identical across protocols at a given (n, seed).
