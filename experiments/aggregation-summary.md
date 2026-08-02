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
| dv-dtn, gossiped link-state | 52.4 ± 5.7% |
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

### Link-state: excellent routes, hardly any of them

The gossip variant is the most interesting row in the table, because its
failure is entirely on one side of the ledger. Balloons exchange *observations*
— who they can hear — and each computes its own route by searching the map it
assembles. Against proactive dv-dtn:

| | link-state | proactive |
|---|---|---|
| believed depth | **3.15 hops** | 7.12 |
| `dropped_loop` | **0.5** | 72.3 |
| `stall_stale_next_hop` | **0.0** | 50.0 |
| `stall_no_belief` | **34,212** | 1,118 |
| completion | 52.4% | 72.3% |

Every quality measure is better and by a wide margin — the routes it finds are
less than half as long, it never forwards to a dead next hop, and it essentially
cannot form a loop, all of which is exactly what computing a path from a map
should buy. It loses anyway, because **most balloons have no route at all**.

The mechanism is gossip bandwidth, and the `lsa` dial confirms it directly.
`lsa` is how many observations ride one transmission — the same
information-per-transmission lever as `mesh`, applied to discovery instead of
to payload:

| lsa | completion | believed depth | `stall_no_belief` |
|---|---|---|---|
| 2 | 41.0 ± 4.3% | 2.67 | 40,547 |
| 4 (default) | 52.3 ± 5.7% | 3.15 | 34,207 |
| 16 | 68.0 ± 6.4% | 4.13 | 22,363 |
| 64 | 71.6 ± 6.9% | 5.57 | 8,067 |

Monotone in both directions at once: more gossip per slot means fewer balloons
stranded without a map, *and* longer routes, because a fuller map can see paths
that a partial one simply did not contain. At `lsa = 64` link-state converges on
proactive dv-dtn's 72.3% — it needs 64 records per transmission to match what a
hop count achieves with one number.

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
| dv-dtn link-state | 48.2 ± 5.4 | 48.4 ± 4.7 | +0.2 ± 1.3 |
| spray-and-wait L=4 | 9.4 ± 1.7 | 10.2 ± 1.6 | +0.8 ± 0.4 |
| spray-and-wait L=16 | 20.4 ± 3.1 | 21.8 ± 3.2 | +1.4 ± 0.7 |

**Compared within every (wind, seed) cell, dv-dtn beats spray-and-wait L=16 in
160 of 160 cells.** The narrowest margin anywhere is **+38.0 points**. This is
not a close call that weather might tip.

**The churn is real and it reached the protocol.** `link_churn` measures link
turnover rising 4.1× (0.091% → 0.376% per round, half-life 757 → 184 rounds) at
unchanged density, and dv-dtn's own counter agrees: `stall_stale_next_hop` goes
42 → ~197, a 4.7× rise, consistent across all seven fields. Mean degree does not
move (6.22 → 6.22). Routes go stale nearly five times as often and delivery
does not care.

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

**Data:** [`discovery-sweep-results.csv`](discovery-sweep-results.csv) — 11
variants × 20 seeds, n = 1200, 400 rounds, zero wind. **Generator:**
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

Combining both is worse than overhearing alone (−1.04 on delivered/orig): the
ring delays the very replies overhearing wants to spread.

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
| MPR at `lsa=4` | +0.64 ± 0.20 | +0.47 ± 0.16 |
| MPR at `lsa=16` | +0.87 ± 0.18 | +0.55 ± 0.15 |

Real, consistent, growing with how much redundancy there was to remove — and
**about a tenth of what was predicted.** The prediction, made in this repo from
the redundancy measurement, was +8-10 points at `lsa=4`.

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

Rows append as they finish and are skipped on restart, so the run can be
interrupted, resumed, or extended with more seeds by re-running with a larger
first argument.
