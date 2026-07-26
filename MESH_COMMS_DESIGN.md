# Tamper-evident mesh comms: design/vision

Design notes for having balloons send **tamper-evident** telemetry to ground towers through the
balloon mesh. The organizing principle is that balloons must **discover** their own connectivity
from signals they actually receive, rather than being handed a globally-computed answer — and that
real cryptography is demonstrated **on demand** for a selected balloon, so the live sim stays
cheap. Companion to `BALLOON_PHYSICS_VISION.md` (the physics half, independent of this one — see
§1 for the two places this document depends on it), `WEATHER_BACKEND_PLAN.md`, and
`RUST_SIM_PLAN.md`.

## Where things stand today

The "network" is undirected line-of-sight edges with a per-edge `grounded` boolean (the connected
component contains a tower). An **offline** experiment binary,
`sim-server/src/bin/connectivity_sweep.rs`, already models per-balloon payload generation, radio
delivery on sustained grounding, and satellite fallback — but as aggregate accounting, with no
per-hop routing, message identity, or signing. There is no log/message/tamper UI anywhere.

Groundwork that already exists: `sim-server/src/bin/mesh_depth.rs` (offline probe measuring the
mesh's graph statistics — see §1.1), a live **mesh-health readout** in the Controls panel (mean
node degree + % grounded, computed in `sim.rs`'s link-recompute block and carried on every
`Snapshot`), and **C1** — the beacon protocol itself, in `sim-server/src/beacon.rs` with the
belief-vs-truth overlay in `src/main.js`.

The vision below adds: (1) a **decentralized** store-and-forward mesh comms protocol in which
balloons learn their own connectivity, (2) tamper-evident message integrity via real crypto with
key rotation, and (3) logging + visualization — with the *crypto* (not the routing) shown on
demand for the inspected balloon so cost stays bounded.

## 1. Mesh → tower comms protocol (decentralized discovery + store-and-forward)

**Data model.** Each balloon periodically emits a **telemetry bundle**: `{origin_id, seq,
created_at, position, alt, gas, ballast, health, ttl}` plus an **environmental sensor block**
`{temperature, pressure, humidity}`. Temperature and pressure come straight from the
`atmosphere.rs` ISA model at the balloon's current altitude (§1 of `BALLOON_PHYSICS_VISION.md`
computes them for the physics; until that exists they can be stubbed); humidity is synthesized
(e.g., a decreasing-with-altitude profile with noise, since ERA5 humidity isn't in the current
wind-only dataset — a natural future tie-in to the "extra variables" idea in
`WEATHER_BACKEND_PLAN.md`). Destination is not "any grounded tower" — the balloon has no way to
know which towers are grounded. It is "whichever tower this balloon currently *believes* it has a
route to," which may be wrong or absent.

**The core principle: no balloon is told it is connected.** A balloon has no access to the edge
graph, the union-find, or the `grounded` boolean. Its entire belief about the network is built
from signals that physically arrived. Before any signal arrives, being deep inside a healthy mesh
and being alone over the Pacific are *indistinguishable* to it. This is the difference between
simulating a network and simulating knowledge of a network, and it's the point of the exercise.

**Discovery — tower beacons flooded outward.** Each tower periodically emits a beacon
`{tower_id, epoch, hop_count: 0}`. Any balloon that hears one records a **route belief**:

```
{ tower_id, hop_count, next_hop: <neighbor it heard from>, epoch, emitted_at_round }
```

and rebroadcasts with `hop_count + 1` — but **only if the belief improved** (lower hop count, or
same count from a fresher wave). That suppression is what keeps a flood from becoming a storm;
without it every balloon re-broadcasts every beacon forever. A balloon with no unexpired belief
considers itself **ungrounded**, and behaves accordingly.

**Beliefs expire — on the age of the news, not on when it was last repeated.** Each beacon carries
the round its tower emitted it; relays copy that stamp verbatim and may never refresh it. A belief
is dropped once that stamp is older than `BELIEF_MAX_AGE_ROUNDS`, and the balloon reverts to "I
don't know of any route" — even if the graph in fact still connects it by some path it hasn't been
told about.

The obvious alternative — expire a belief if you haven't *heard* one lately — is broken, and
measurably so: any relay of stale news renews its lease, so a cluster cut off from every tower
sustains dead routes indefinitely by passing them in circles. Implementing it that way produced a
field that never forgot, holding ~60% of balloons in a confident belief in routes that did not
exist, *with every tower deleted from the world*. This is OSPF's LSA MaxAge distinction, and it is
easy to get wrong. **Belief and ground truth are allowed to diverge, in both directions**: a
balloon can think it's grounded moments after its link broke, and can think it's isolated while
sitting on a perfectly good path. Those divergences are the interesting behavior, not bugs to be
engineered away. (The server can still compute truth via union-find — it's needed for the edge
rendering anyway — which makes belief-vs-truth directly visualizable; see §3.)

**Bundles carry their own path history.** When a balloon has telemetry to send, it forwards to
its currently-believed `next_hop` — a local, possibly stale, possibly suboptimal choice, *not* a
globally-optimal path. Each relay **appends its own id** to the bundle before passing it on. That
one mechanism buys three things at once: loop detection (drop a bundle that reaches a balloon
already in its path), a TTL/hop budget, and the provenance needed for the relay attestations in
§2. Single-path forwarding (not epidemic) is recommended — legible to visualize, and it keeps
dedup simple.

**Acks are source-routed back along the recorded path.** On arrival at a tower, the tower emits an
ack referencing the bundle's recorded hop list, reversed. This is what closes the loop for the
originating balloon: not "the server says you're grounded," but "a signed ack came back through
hops I can name." The failure mode is the good part — the reverse path may have broken while the
bundle was in flight, so **acks can be lost even though delivery succeeded**. A balloon then
cannot distinguish "my data never arrived" from "it arrived and the ack died," which is a real
distributed-systems problem and worth surfacing in the UI rather than papering over. See §4.

**Satellite fallback.** When a bundle ages past a timeout with no ack, satellite delivers it
directly (reusing the policy already in `connectivity_sweep.rs`). Per §1.1 this is the *common*
case at many slider settings, not a rare edge case.

### 1.1 Measured mesh statistics (what sets the constants)

`sim-server/src/bin/mesh_depth.rs` measures the graph offline: multi-source BFS from all towers
over the real `link_detection` output, swept across balloon count × horizon coefficient. Results
(validated against the live server to within ~1%):

- **Mean node degree is the real control variable.** The balloon-count and horizon-coefficient
  sliders are two different ways of moving the same quantity. Read by degree, the sweep collapses
  onto a single curve regardless of which knob produced it.
- **The mesh percolates at degree ≈ 4.5** — the 2D continuum-percolation threshold, reproduced.
  Below it the network shatters into islands (at degree 2.1, ~89% of balloons can *never* reach a
  tower); above ~5 nearly everything is grounded. The transition is sharp.
- **Path depth peaks _at_ the threshold**, not at low density: median ~14 and max ~50 hops right
  at criticality, versus median ~5 in a healthy mesh. Long paths are a symptom of criticality.
- **Edges are long-lived.** A balloon drifts ~5.4 km per 3-sim-minute link round against link
  ranges of ~600–1300 km — **0.4%**. Topology is quasi-static on any sane comms timescale.

Two consequences for the protocol. First, **the shattered regime is reachable at every slider
position** (at horizon 2.5 even the full 2000-balloon pool barely percolates), so "no route
exists and none will" must be a first-class case, not an afterthought. Second, since edges are
quasi-static, beacons would converge almost instantly if propagation were free — which would make
every balloon's belief exactly equal the `grounded` boolean and render the whole design pointless.

**What makes discovery slow is duty cycling, not propagation delay.** Real HAB radios can't
transmit continuously on their power budget; they wake, beacon, and sleep. A link can exist
geometrically and still carry nothing because both ends were asleep. That is a physical constraint
rather than a fudge factor, and it is the parameter that makes belief lag truth.

### 1.2 The comms clock — why the protocol has its own

Comms constants are denominated in **rounds**, not ticks. A round is `COMMS_EVERY_N_TICKS`
world ticks, exactly the pattern `LINK_UPDATE_EVERY_N_TICKS` already uses for link detection.

This is not cosmetic. `TICK_INTERVAL_MS` has to serve two unrelated jobs: it is the snapshot
rate — which must stay near 20 Hz, since the client sets balloon positions directly per snapshot
with no interpolation, so anything slower visibly stutters — and it *was* also the protocol clock.
At `TIME_SCALE = 60` that pinned the whole protocol to **1200× real time**, and none of the
behavior this design exists to show was observable: the measured discovery arc crossed the planet
in **2.4 real seconds** and belief expiry took **3**. Choices that look weighty in simulated
minutes (does a bundle advance one hop per tick or one per duty cycle?) were a choice between
0.7 s and 3.5 s on screen — i.e. between invisible and invisible.

Separating the clocks makes the protocol's real-time pace tunable without touching physics
smoothness. Retune **`COMMS_EVERY_N_TICKS` for pacing**; if the simulated-time durations then look
implausible, adjust `TIME_SCALE`, which trades balloon drift speed for them. Do **not** reach for
`BELIEF_MAX_AGE_ROUNDS` — it is pinned from below by measured convergence (below), not free.

Starting constants:

| Parameter | Value | Rationale |
|---|---|---|
| Comms round | 8 ticks (~8 sim min, 0.4 real s) | Pacing dial. Puts the discovery arc at ~20 real seconds and belief expiry at ~24 — slow enough to watch a wavefront spread and a stale belief die. |
| Beacon interval | 5 rounds, jittered ±20% | Duty cycle. Decoupled from the link-recompute cadence so the comms and topology clocks don't beat against each other; jitter avoids lockstep rebroadcast collisions. |
| Belief max age | 60 rounds (~12× beacon interval) | Measured, not chosen: expiry keys on emission time, so this must exceed the time a wave needs to cross the mesh (~50 rounds to reach 99% of a 1200-balloon field) or deep balloons expire beliefs on arrival and can never hold a route. A contact-recency timeout of 3–4× would have been shorter, but is unsound — see §1. |
| Bundle TTL / max hops | ~20 | Covers p95 depth at operational densities; deliberately truncates the critical-regime tails, where handing off to satellite is the correct policy anyway. |
| Ack timeout → satellite | a few beacon intervals | Must exceed a plausible round trip (2× path traversal), or satellite will fire while the ack is still legitimately in flight. |

One consequence worth noting: at 8 ticks per round, topology churn is no longer negligible over a
belief's lifetime. `beacon_convergence.rs` now shows a persistent 0.4–1.1% stale fraction even in
its nominally "frozen" zero-wind phase, where it used to read exactly 0.0%. That is the altitude
random walk breaking links faster than beliefs expire — real behavior surfacing at a realistic
clock, not a regression.

## 2. Tamper-evidence — real crypto, verified on demand

**Identity.** Each balloon owns an **Ed25519 keypair** (generated at spawn; public keys
registered in a ground-side directory). Towers/ground know every balloon's public key.

**Key rotation (periodic or on demand).** Keys are not fixed for the mission — a balloon can roll
its keypair on a schedule *or* when commanded (the `RotateKey{id}` uplink command, §2 of
`BALLOON_PHYSICS_VISION.md`). Rotation is an authenticated handover so trust survives the change:
the balloon generates a new keypair and emits a **rotation certificate** — the *new* public key
signed by the *old* private key — which propagates to the ground directory. The directory keeps a
per-balloon **key history (epochs)**; every telemetry record notes the key epoch that signed it, so
records remain verifiable across rotations and a rotation that *isn't* signed by the prior key is
itself flagged as suspicious. This demonstrates forward key management (limiting the blast radius
of a compromised key) on top of the tamper-evident log.

**Tamper-evident log (hash chain + signatures).** Each balloon maintains an **append-only hash
chain** of its telemetry records:

```
record_i.hash = SHA-256( record_i.data || record_{i-1}.hash )   // chain of custody
record_i.sig  = Ed25519_sign( sk_balloon, record_i.hash )
```

Any modification to a past record breaks every subsequent hash and invalidates the signature —
**provably** detectable. Each forwarding balloon additionally signs a "relayed-by" attestation
over the bundle's path history from §1, turning that recorded hop list into a *verifiable* one —
a malicious relay that alters a bundle in transit, or fabricates a hop it didn't make, is caught.

**On-demand verification — and what that rule now covers.** Routing is *not* on demand: beacon
relaxation is one O(V+E) pass per round for the whole field, and per-balloon belief state is four
fields. That is cheaper than the per-query BFS this document originally proposed, so connectivity
knowledge is always-on. **Cryptography is the genuinely expensive part, and it alone is bounded.**
The chain hash is cheap and maintained continuously; **Ed25519 signing/verification is done for
the selected balloon only** — when queried, the server (acting as the tower) verifies that
balloon's chain + signatures and returns per-record status: `verified | bad-signature |
broken-link`. A UI control can **inject a tamper** into a stored record to demonstrate the chain
flipping red.

Design note: to be fully authentic, signing must happen at record *creation*. To keep creation
cheap at scale, sign only a bounded recent window per balloon, or sign lazily and be explicit that
it's a demonstration. Recommended crates: `ed25519-dalek`, `sha2`.

## 3. Logging & visualization

**Server.** A bounded per-balloon comms event log (created / forwarded / delivered / satellite /
tampered / ack-received / ack-timed-out). Lightweight status rides in the existing `Snapshot`
(new per-balloon fields: pending/delivered/satellite counts, **believed** hop-count-to-tower and
belief age, `last_channel` (satellite/radio/none), chain-head hash, verified flag, current key
epoch). Rich detail comes from the on-demand `GET /api/balloons/:id/comms` (belief state + message
log + path histories + verification).

**Already built:** the Controls panel shows live **mean node degree** (colored against the ~4.5
percolation threshold, with a tick mark at it) and **% grounded**. Since both existing sliders
secretly move mean degree, this keeps a drag from walking blindly across the phase transition —
which would otherwise make comms behavior look inexplicably bimodal. The **belief-vs-truth
overlay** shipped with C1.

**Frontend** (patterns already in the codebase):
- **Belief vs. truth** — the visualization this design exists to enable. The server knows both:
  ground truth from union-find, and each balloon's belief from the beacon protocol. Render the
  disagreement — e.g. balloons that *think* they're grounded but aren't (and vice versa) — as a
  distinct color or badge. Watching a stale belief persist after a link breaks, then expire, is
  the single most legible demonstration that this is real distributed discovery.
- **Beacon wavefront** (toggle): animate a beacon flooding outward from a tower, hop by hop, so
  the propagation delay that drives everything else is visible rather than inferred.
- **Select a balloon** (click) → query endpoint → render:
  - **Animated packet** hopping node-to-node along the bundle's *actual recorded path* (not a
    recomputed shortest path — the whole point is that the two can differ), reusing the
    reused-`PolylineCollection`-keyed-by-`pairKey` pattern from `syncLinks` in `src/main.js`
    (a moving billboard/point advancing one edge per interval). Animate the **ack returning**
    along the reverse path, including the case where it dies partway.
  - A **comms log panel/table** (net-new DOM; the Controls panel is the only precedent): rows of
    `time · seq · hash-prefix · #hops · channel(radio/satellite) · ack state · tamper status`.
    Ack state must distinguish **delivered-and-acked** from **delivered-but-ack-lost** from
    **unknown** — from the balloon's own perspective the latter two are identical, and showing
    that gap next to the server's ground truth is the lesson. See §4.
- **Command uplink viz**: satellite downlink beam vs. highlighted radio mesh path when a command
  is sent to the selected balloon.
- **Last-delivery indicator on the balloon**: each balloon carries a small badge showing *how its
  most recent data got through* — a **satellite glyph** (delivered via satellite) vs. a **tower
  glyph** (delivered via radio mesh), or equivalently a color code (e.g., blue = satellite,
  lime = radio, gray = pending/undelivered). This reads at a glance across the whole field without
  selecting anything, and reuses the altitude-glyph billboard mechanism already in `src/main.js`
  (`buildBalloonIcon` / per-balloon billboard). `last_channel` rides in the `Snapshot` fields
  above.
- **Tamper demo control**: inject tampering into the selected balloon's chain and watch a record
  flip red with the break point identified.
- **Optional global layer** (toggle, like wind vectors): color balloons by delivery status
  (delivered / pending / satellite-only).

## 4. C2 design decisions (settled, not yet implemented)

Four questions came up scoping C2. Three are decided; the fourth is deliberately deferred until
the comms clock is tuned, because before §1.2 it was not answerable by observation.

**Decided.**

- **Ack loss reuses the C1 belief-vs-truth shape.** Each bundle carries two independent states:
  what the *balloon* can know, and what the *server* knows. The balloon may only ever act on the
  left column.

  | reality | balloon's view | server truth |
  |---|---|---|
  | never arrived | `Pending` → `TimedOut` | `NeverArrived` |
  | arrived, ack died en route | `Pending` → `TimedOut` | `ArrivedAckLost` |
  | arrived and acked | `Acked` | `ArrivedAcked` |

  The two middle-column cells being identical *is* the phenomenon — a balloon genuinely cannot
  distinguish "my data never made it" from "it made it and the receipt died." Rendering that as a
  visible band of disagreement, exactly like the amber/red belief overlay, turns what looked like
  a UI problem into the lesson. It also gives the harness a sharp metric: ack-loss rate versus
  mean degree, expected to peak near percolation where paths are longest.

- **One outstanding bundle per balloon.** A balloon emits its next bundle only once the previous
  one is acked or times out. Bounded memory at 2000 balloons, matches a real store-and-forward
  buffer, and keeps the counters interpretable. Free-running emission mostly buys queue-management
  complexity.

- **A stale next-hop holds, it does not drop.** The believed `next_hop` will frequently no longer
  be a neighbour. The bundle waits in the balloon's buffer until its belief refreshes or the TTL
  expires. This is what makes the network delay-tolerant rather than merely lossy, and holding is
  what generates the interesting late deliveries.

**Open.**

- **Does a bundle advance one hop per tick, or one hop per duty-cycle slot?** The duty-cycle
  version (reusing the balloon's beacon slot, since a radio that is awake is awake for both) is
  the coherent physical story and makes ack timeouts meaningful, since beliefs can then expire
  mid-flight. The per-tick version delivers roughly 5× faster. Settle it by watching both once
  `COMMS_EVERY_N_TICKS` is tuned — before §1.2 the two were 0.7 s and 3.5 s on screen, i.e.
  indistinguishable.

## Suggested phasing

**This track does not depend on the physics track** (`BALLOON_PHYSICS_VISION.md`). The only
couplings are the environmental sensor block in §1's telemetry bundle, which can carry stubbed
temperature/pressure until `atmosphere.rs` exists, and the `RotateKey` command §2 borrows. So the
two can be done in either order, or interleaved.

- **C1.** Beacon protocol: per-balloon route belief, tower beacons, rebroadcast suppression,
  staleness expiry. Ships with the belief-vs-truth overlay (§3) — visually demonstrable on its own,
  and it is the piece that validates the §1.1 constants against the live sim.
- **C2.** Telemetry bundles with path history + forwarding along believed next-hop + loop/TTL
  handling; tower acks source-routed back; satellite fallback on ack timeout (port the
  `connectivity_sweep` policy). Aggregate counters into `Snapshot`. **Design settled — see §4.**
- **C3.** Hash chain + Ed25519 signing/verification + key rotation, behind
  `GET /api/balloons/:id/comms`.
- **C4.** Frontend: selection, animated packet along recorded path, ack animation, comms log panel,
  tamper-demo, last-delivery glyph.

Each phase is independently reviewable and visually demonstrable. **C1 is done** — beacon
protocol, belief expiry, and the belief-vs-truth overlay are in `sim-server/src/beacon.rs` and the
Controls panel. C2 is the next step.

**Verification convention (learned the hard way in C1).** Every comms phase needs an offline
harness under `sim-server/src/bin/`, not just a look at the globe. `sim-server` is a library, so a
binary can drive a real `World` with `WindField::zero()` and run hundreds of rounds in
milliseconds. The properties that matter here are temporal — does belief converge, does it decay,
does it decay *completely* — and none of them are visible in a screenshot. C1's protocol bug (a
belief-refresh rule that let cut-off clusters sustain dead routes forever) only surfaced in a
scenario with every tower deleted from the world, which is trivial offline and impossible to stage
by clicking. `bin/beacon_convergence.rs` is the template.

C2 needs `bin/bundle_delivery.rs`, with C1's phase-3 move built in from the start: remove every
tower and assert every bundle *resolves* — delivered, TTL-expired, or handed to satellite — with
nothing held forever and no unbounded queue growth. That is the same class of bug as C1's
self-sustaining beliefs, and equally invisible in a browser.

## Reused existing pieces

- Delivery + satellite-fallback policy: `sim-server/src/bin/connectivity_sweep.rs`.
- Grounded-component / union-find primitives: `sim-server/src/link_detection.rs`,
  `sim-server/src/union_find.rs`. Note the role change: these stay the engine's **ground truth**
  (and drive edge rendering + the mesh-health readout), but must never be the source of a
  balloon's own belief about being grounded — see §1.
- Mesh graph statistics + the measurement harness behind §1.1: `sim-server/src/bin/mesh_depth.rs`.
- Command/REST + Snapshot protocol to extend: `sim-server/src/sim.rs`, `sim-server/src/main.rs`.
- Reused-`PolylineCollection` + reconcile patterns and the Controls-panel-building convention:
  `src/main.js`.
