# Aggregation: batching mesh hops, and acks that cost nothing

> **Status: provisional — single seed.** Every number below comes from one
> balloon field. This simulation's run-to-run variance is dominated by how many
> balloons happen to sit in tower range, which correlates with delivered/round
> at r = 0.94, so a single seed can resolve a large effect and nothing finer.
> The multi-seed replacement is described under [Confirming this](#confirming-this)
> and should overwrite these tables before any figure here is quoted elsewhere.

**Data:** single-seed output of [`batch_sweep`](../sim-server/src/bin/batch_sweep.rs)
(`batch_sweep 1200 600`) · **Generator for the confirming run:**
[`aggregation_sweep`](../sim-server/src/bin/aggregation_sweep.rs) ·
**Tables from:** [`summarize_aggregation.py`](summarize_aggregation.py)

## The question

[`MESH_COMMS_DESIGN.md`](../docs/design/MESH_COMMS_DESIGN.md) §4 closed a long
investigation with the finding that delivery is limited by the **last hop**:
only ~23 of 1200 balloons can hear a tower at any moment, and sweeping offered
load 28× left delivery pinned near 3.1/round. Widening the tower contact window
from 1 to 4 bought +16 points of completion and then saturated, at which point
the doc noted the limit had moved *back into the mesh* — and stopped there:
"the remaining loss is now a different problem from the one investigated here."

This is that different problem. Both levers here raise **information per
transmission** rather than transmissions per second, which is the one direction
the earlier work never tried:

- **Batching** (`BatchPolicy`) generalizes the tower-contact window to
  balloon-to-balloon hops, so one wake slot can carry several bundles.
- **The ack digest** (`AckPolicy::Digest`) stops sending receipts as packets
  at all. Towers announce recent deliveries inside beacons they were already
  transmitting, and the announcement floods outward with the wave.

Both are transmission-time changes only. A batched hop still carries K
individually identified bundles, each with its own origin, seq and recorded
path — so per-record provenance survives for C3's signing, and `delivered`
still counts records.

## Results

n = 1200, coeff 4.12, 600 rounds, zero wind, seed 42. Completion is
delivered/resolved; `blocked` counts handoffs refused by a full receiver
(retried, not lost).

| acks | mesh | tower | completion | deliv/round | ceiling | blocked | ack_lost |
|---|---|---|---|---|---|---|---|
| source-routed | 1 | 4 | **66.0%** | 3.30 | 22.68 | 2915 | 601 |
| source-routed | 4 | 4 | 90.0% | 4.80 | 22.68 | 5194 | 1367 |
| digest | 1 | 4 | 79.5% | 4.14 | 22.68 | 1939 | 0 |
| digest | 4 | 4 | **95.7%** | 5.31 | 22.68 | 1246 | 0 |

The first row is what ships. The last is both levers together.

### The digest is close to free, and pays twice

Switching acks to the digest at otherwise-shipped settings moves completion
**66.0% → 79.5%**. It does this while *removing* traffic: `ack_lost` goes to
zero by construction, because no ack packet is ever created to be lost.

The second payment is the interesting one. `blocked` **falls**, 2915 → 1939.
Acks were not merely occupying airtime, they were winning it — `bundle.rs`
deliberately gives a relayed ack priority over that balloon's own forwarding,
since letting both ride one wake would double its per-slot throughput and
undermine the scarcity the whole model rests on. Removing acks as packets hands
those slots back to ordinary forwarding, so the mesh drains faster as a side
effect of the receipt getting cheaper.

This is close to DTN's **Aggregate Custody Signals**, which exist for the same
reason: per-bundle custody signals were too expensive to send individually.

### Batching only pays once the ground link isn't the cap

At `tower_contact = 1`, where the last hop is still the binding constraint,
batching mesh hops buys little and costs a lot:

| acks | mesh | tower | completion | blocked |
|---|---|---|---|---|
| source-routed | 1 | 1 | 57.9% | 3562 |
| source-routed | 8 | 1 | 66.9% | 24495 |

+9 points for a **7× rise in blocked handoffs**. The mesh pushes harder into a
ground link that cannot take more, and the bundles pile up one hop short. At
`tower_contact = 4` the same change is worth +24 points (66.0% → 90.0%) at less
than double the blocking.

So the two levers are not independent, and neither alone is enough: tower=4
with mesh=1 gives 66.0%, tower=1 with mesh=8 gives 66.9%. Together with the
digest, 95.7%. **The last hop has to stop binding before mesh airtime becomes
worth spending.**

### What still limits it

Delivery reaches 5.31/round against a last-hop ceiling of 22.68 — the ground
link is now far from saturated, and offered load at `originate = 200` is about
6/round. At 95.7% completion the mesh is close to carrying everything it is
asked to carry, so the next honest experiment is to raise demand rather than
capacity, and find where it breaks next.

## Why the comparison is trustworthy even at one seed

Rows differing only in protocol parameters ran over an **identical balloon
field**. The protocol draws from an RNG stream independent of the world's
(`MeshProtocol::reseed`), so changing `mesh`, `tower` or `ack` cannot perturb a
single spawn position or altitude drift — there is a test asserting exactly
that (`protocol_choice_does_not_perturb_the_balloon_field`).

That makes *differences between these rows* clean. It does not make the
*levels* general: this is one field, and the field is what the dominant
variance comes from. A different seed will move all eight rows together.

## Confirming this

[`aggregation_sweep`](../sim-server/src/bin/aggregation_sweep.rs) runs the same
grid across many seeds and three densities:

```bash
cd sim-server
RAYON_NUM_THREADS=3 nohup ./target/release/aggregation_sweep 24 800 \
  > /tmp/aggregation-sweep.log 2>&1 &
```

~4.5h on four cores. Rows append to
`experiments/aggregation-sweep-results.csv` as they finish and are skipped on
restart, so it can be interrupted, resumed, or extended with more seeds by
re-running with a larger first argument.

Then:

```bash
python3 experiments/summarize_aggregation.py experiments/aggregation-sweep-results.csv
```

which prints the tables to paste in here. Its headline analysis is **paired**:
because every configuration at a given seed sees the same field, differences
are taken per seed before averaging, which cancels the between-field variance
rather than averaging over it. Expect the paired effect estimates to be far
tighter than the spread of the levels would suggest.

**When that run lands, replace the Results section above** with the paired
tables and drop this provisional banner.

## Limitations

- **Zero wind.** Topology is near-frozen, so bundles are not chasing a moving
  target. Real ERA5 wind churns links and should hurt every row, plausibly not
  equally.
- **One density** in the tables above (n = 1200, degree ~6.3, above
  percolation). The multi-seed run covers 600/1200/2000.
- **`originate = 200` throughout.** Both levers raise capacity; none of this
  says where the mesh breaks under heavier demand.
- **Truncation of the digest is untested at scale.** At `digest_entries = 16`
  and these delivery rates the window is generous. A denser field delivering
  faster could overflow it, at which point origins would learn late rather than
  never — worth measuring before the parameter is trusted.
