# Balloon physics + tamper-evident mesh comms: design/vision

Design notes (thinking, not yet an implementation plan) for two related directions: making
balloon vertical dynamics physically real, and having balloons send **tamper-evident** telemetry
to ground towers through the balloon mesh. The organizing principle for the comms half is that
balloons must **discover** their own connectivity from signals they actually receive, rather than
being handed a globally-computed answer — and that real cryptography is demonstrated **on demand**
for a selected balloon, so the live sim stays cheap. Companion to `WEATHER_BACKEND_PLAN.md` and
`RUST_SIM_PLAN.md`.

## Where things stand today

The simulator models balloons minimally: **pure wind advection** horizontally and a
**proportional "thermostat"** vertically that drives altitude toward a randomly-drifting target
(`sim-server/src/balloon.rs`). There is no mass, gas, ballast, buoyancy force, or per-balloon
command input; only `id/lon/lat/alt` reach the frontend. The "network" is undirected
line-of-sight edges with a per-edge `grounded` boolean (the connected component contains a
tower). An **offline** experiment binary, `sim-server/src/bin/connectivity_sweep.rs`, already
models per-balloon payload generation, radio delivery on sustained grounding, and satellite
fallback — but as aggregate accounting, with no per-hop routing, message identity, or signing.
There is no log/message/tamper UI anywhere.

Two pieces of groundwork for the comms design already exist: `sim-server/src/bin/mesh_depth.rs`
(offline probe measuring the mesh's graph statistics — see §3.1) and a live **mesh-health readout**
in the Controls panel (mean node degree + % grounded, computed in `sim.rs`'s link-recompute block
and carried on every `Snapshot`).

The vision below adds: (1) realistic vertical dynamics driven by ballast + gas, (2) a command
uplink (satellite/radio), (3) a **decentralized** store-and-forward mesh comms protocol in which
balloons learn their own connectivity, (4) tamper-evident message integrity via real crypto with
key rotation, and (5) logging + visualization — with the *crypto* (not the routing) shown on
demand for the inspected balloon so cost stays bounded.

## 1. Realistic vertical physics (recommended: real buoyancy model)

Model a **zero-pressure high-altitude balloon** — the archetype that matches "ballast and vents
gas." Replace the direct-velocity P-controller in `balloon.step()` with force integration.

New `Balloon` state (extends the current struct):
- `gas_moles` (n) — lift gas quantity. Venting reduces it, irreversibly.
- `ballast_kg` — droppable mass. Dropping reduces it, irreversibly.
- `mass_dry_kg` — fixed envelope + payload mass.
- `v_z` — vertical velocity (now integrated, not a controller output).
- `envelope_vmax_m3` — max envelope volume (zero-pressure cap).

Per-tick vertical update, with atmosphere as a function of altitude via an **ISA model** —
pressure `P(h)`, temperature `T(h)`, density `ρ(h)` — mirroring the piecewise troposphere +
isothermal-stratosphere math already in `weather-data-server/wind_backend.py`, ported to Rust in
a new `atmosphere.rs`:

```
V_gas = min(n·R·T(h) / P(h),  envelope_vmax)        // ideal gas; zero-pressure cap
if n·R·T/P > envelope_vmax:  n -= (auto-vent excess) // zero-pressure balloons spill gas at float
F_buoy = ρ(h) · V_gas · g
m_tot  = mass_dry + ballast + gas_mass(n)
F_grav = m_tot · g
F_drag = ½ · ρ(h) · Cd · A · v_z · |v_z|             // opposes motion
a_z    = (F_buoy − F_grav − F_drag) / m_tot
v_z   += a_z · dt ;  alt += v_z · dt                 // clamped to [MIN, MAX]
```

This yields a **natural float altitude** (where net force ≈ 0 after auto-venting) instead of a
magic setpoint, and makes the actuators physical:
- **Drop ballast** → mass ↓ → rises to a new float altitude. Finite.
- **Vent gas** → n ↓ → V_gas ↓ → buoyancy ↓ → descends. Finite and irreversible.

A balloon that exhausts ballast can no longer climb; one that over-vents sinks permanently — the
realistic constraint that gives the sim stakes and makes command decisions matter.

**Onboard autopilot** keeps the old role (track a commanded target altitude) but now acts through
these finite actuators with a **deadband/hysteresis** (pulse ballast/gas only when the error
leaves a band) so it doesn't waste resources — matching how real HAB/Loon controllers behave.

Why this over a lighter "actuator-on-thermostat" layer: it's only modestly more code once
`atmosphere.rs` exists, and it's the version where ballast/gas are genuinely meaningful. The
lighter option remains a fallback if budget forces it.

## 2. Command uplink — "instructions from satellite or radio"

New per-balloon commands (extend the `Command` enum + REST, mirroring the existing
`AddTower`/`SetHorizonRefractionCoeff` endpoints in `sim-server/src/main.rs`):
`SetTargetAltitude{id, alt}`, `DropBallast{id, kg}`, `VentGas{id, moles}`, and `RotateKey{id}`
(§4 key rotation).

Two delivery channels, which the sim distinguishes (and the UI visualizes):
- **Satellite**: always reachable (optionally with a latency), can command any balloon anytime.
- **Radio**: only when the balloon is currently reachable from a tower through the mesh (its
  component is `grounded`). The command rides *down* the mesh — the reverse of telemetry going up.

Each executed command records which channel delivered it, so the UI can show a satellite downlink
beam vs. a highlighted radio path through the mesh.

## 3. Mesh → tower comms protocol (decentralized discovery + store-and-forward)

**Data model.** Each balloon periodically emits a **telemetry bundle**: `{origin_id, seq,
created_at, position, alt, gas, ballast, health, ttl}` plus an **environmental sensor block**
`{temperature, pressure, humidity}`. Temperature and pressure come straight from the
`atmosphere.rs` ISA model at the balloon's current altitude (§1 already computes them for the
physics); humidity is synthesized (e.g., a decreasing-with-altitude profile with noise, since
ERA5 humidity isn't in the current wind-only dataset — a natural future tie-in to the "extra
variables" idea in `WEATHER_BACKEND_PLAN.md`). Destination is not "any grounded tower" — the
balloon has no way to know which towers are grounded. It is "whichever tower this balloon
currently *believes* it has a route to," which may be wrong or absent.

**The core principle: no balloon is told it is connected.** A balloon has no access to the edge
graph, the union-find, or the `grounded` boolean. Its entire belief about the network is built
from signals that physically arrived. Before any signal arrives, being deep inside a healthy mesh
and being alone over the Pacific are *indistinguishable* to it. This is the difference between
simulating a network and simulating knowledge of a network, and it's the point of the exercise.

**Discovery — tower beacons flooded outward.** Each tower periodically emits a beacon
`{tower_id, epoch, hop_count: 0}`. Any balloon that hears one records a **route belief**:

```
{ tower_id, hop_count, next_hop: <neighbor it heard from>, heard_at: <tick> }
```

and rebroadcasts with `hop_count + 1` — but **only if the belief improved** (lower hop count, or
same count from a fresher epoch). That suppression is what keeps a flood from becoming a storm;
without it every balloon re-broadcasts every beacon forever. A balloon with no unexpired belief
considers itself **ungrounded**, and behaves accordingly.

**Beliefs expire.** Each belief carries a staleness timeout. Miss enough beacon intervals and it
is dropped, and the balloon reverts to "I don't know of any route" — even if the graph in fact
still connects it by some path it hasn't been told about. **Belief and ground truth are allowed
to diverge, in both directions**: a balloon can think it's grounded moments after its link broke,
and can think it's isolated while sitting on a perfectly good path. Those divergences are the
interesting behavior, not bugs to be engineered away. (The server can still compute truth via
union-find — it's needed for the edge rendering anyway — which makes belief-vs-truth directly
visualizable; see §5.)

**Bundles carry their own path history.** When a balloon has telemetry to send, it forwards to
its currently-believed `next_hop` — a local, possibly stale, possibly suboptimal choice, *not* a
globally-optimal path. Each relay **appends its own id** to the bundle before passing it on. That
one mechanism buys three things at once: loop detection (drop a bundle that reaches a balloon
already in its path), a TTL/hop budget, and the provenance needed for the relay attestations in
§4. Single-path forwarding (not epidemic) is recommended — legible to visualize, and it keeps
dedup simple.

**Acks are source-routed back along the recorded path.** On arrival at a tower, the tower emits an
ack referencing the bundle's recorded hop list, reversed. This is what closes the loop for the
originating balloon: not "the server says you're grounded," but "a signed ack came back through
hops I can name." The failure mode is the good part — the reverse path may have broken while the
bundle was in flight, so **acks can be lost even though delivery succeeded**. A balloon then
cannot distinguish "my data never arrived" from "it arrived and the ack died," which is a real
distributed-systems problem and worth surfacing in the UI rather than papering over.

**Satellite fallback.** When a bundle ages past a timeout with no ack, satellite delivers it
directly (reusing the policy already in `connectivity_sweep.rs`). Per §3.1 this is the *common*
case at many slider settings, not a rare edge case.

### 3.1 Measured mesh statistics (what sets the constants)

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

Proposed starting constants:

| Parameter | Value | Rationale |
|---|---|---|
| Beacon interval | ~5 sim min, jittered ±20% | Duty cycle. ~1.7 link rounds, so the comms and topology clocks stay decoupled; jitter avoids lockstep rebroadcast collisions. |
| Belief staleness timeout | 3–4× beacon interval (15–20 sim min) | "Missed 3 beacons → assume gone," the OSPF dead-interval convention. Refresh rate is one interval regardless of depth, so this need not scale with hop count. |
| Bundle TTL / max hops | ~20 | Covers p95 depth at operational densities; deliberately truncates the critical-regime tails, where handing off to satellite is the correct policy anyway. |
| Ack timeout → satellite | a few beacon intervals | Must exceed a plausible round trip (2× path traversal), or satellite will fire while the ack is still legitimately in flight. |

## 4. Tamper-evidence — real crypto, verified on demand

**Identity.** Each balloon owns an **Ed25519 keypair** (generated at spawn; public keys
registered in a ground-side directory). Towers/ground know every balloon's public key.

**Key rotation (periodic or on demand).** Keys are not fixed for the mission — a balloon can roll
its keypair on a schedule *or* when commanded (the `RotateKey{id}` uplink command, §2). Rotation
is an authenticated handover so trust survives the change: the balloon generates a new keypair and
emits a **rotation certificate** — the *new* public key signed by the *old* private key — which
propagates to the ground directory. The directory keeps a per-balloon **key history (epochs)**;
every telemetry record notes the key epoch that signed it, so records remain verifiable across
rotations and a rotation that *isn't* signed by the prior key is itself flagged as suspicious.
This demonstrates forward key management (limiting the blast radius of a compromised key) on top
of the tamper-evident log.

**Tamper-evident log (hash chain + signatures).** Each balloon maintains an **append-only hash
chain** of its telemetry records:

```
record_i.hash = SHA-256( record_i.data || record_{i-1}.hash )   // chain of custody
record_i.sig  = Ed25519_sign( sk_balloon, record_i.hash )
```

Any modification to a past record breaks every subsequent hash and invalidates the signature —
**provably** detectable. Each forwarding balloon additionally signs a "relayed-by" attestation
over the bundle's path history from §3, turning that recorded hop list into a *verifiable* one —
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

## 5. Logging & visualization

**Server.** A bounded per-balloon comms event log (created / forwarded / delivered / satellite /
tampered / ack-received / ack-timed-out). Lightweight status rides in the existing `Snapshot`
(new per-balloon fields: pending/delivered/satellite counts, **believed** hop-count-to-tower and
belief age, `last_channel` (satellite/radio/none), chain-head hash, verified flag, current key
epoch). Rich detail comes from the on-demand `GET /api/balloons/:id/comms` (belief state + message
log + path histories + verification).

**Already built:** the Controls panel shows live **mean node degree** (colored against the ~4.5
percolation threshold, with a tick mark at it) and **% grounded**. Since both existing sliders
secretly move mean degree, this keeps a drag from walking blindly across the phase transition —
which would otherwise make comms behavior look inexplicably bimodal.

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
    that gap next to the server's ground truth is the lesson.
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

## Suggested phasing (when coding is planned later)

**The comms track does not depend on the physics track.** The only coupling is the environmental
sensor block in §3's telemetry bundle, which can carry stubbed temperature/pressure until
`atmosphere.rs` exists and then be swapped to the real ISA values. So the two tracks below can be
done in either order, or interleaved.

*Physics track:*

- **P1.** `atmosphere.rs` + buoyancy physics + ballast/gas state (self-contained; no comms).
- **P2.** Per-balloon command uplink (target alt / drop ballast / vent gas) + satellite vs radio
  channel.

*Comms track:*

- **C1.** Beacon protocol: per-balloon route belief, tower beacons, rebroadcast suppression,
  staleness expiry. Ships with the belief-vs-truth overlay (§5) — visually demonstrable on its own,
  and it is the piece that validates the §3.1 constants against the live sim.
- **C2.** Telemetry bundles with path history + forwarding along believed next-hop + loop/TTL
  handling; tower acks source-routed back; satellite fallback on ack timeout (port the
  `connectivity_sweep` policy). Aggregate counters into `Snapshot`.
- **C3.** Hash chain + Ed25519 signing/verification + key rotation, behind
  `GET /api/balloons/:id/comms`.
- **C4.** Frontend: selection, animated packet along recorded path, ack animation, comms log panel,
  tamper-demo, last-delivery glyph.

Each phase is independently reviewable and visually demonstrable. C1 is the recommended starting
point for the comms work — it is small, self-contained, and everything else builds on the belief
state it establishes.

## Reused existing pieces

- ISA pressure↔altitude math to port into `atmosphere.rs`: `weather-data-server/wind_backend.py`.
- Delivery + satellite-fallback policy: `sim-server/src/bin/connectivity_sweep.rs`.
- Grounded-component / union-find primitives: `sim-server/src/link_detection.rs`,
  `sim-server/src/union_find.rs`. Note the role change: these stay the engine's **ground truth**
  (and drive edge rendering + the mesh-health readout), but must never be the source of a
  balloon's own belief about being grounded — see §3.
- Mesh graph statistics + the measurement harness behind §3.1: `sim-server/src/bin/mesh_depth.rs`.
- Command/REST + Snapshot protocol to extend: `sim-server/src/sim.rs`, `sim-server/src/main.rs`.
- Reused-`PolylineCollection` + reconcile patterns and the Controls-panel-building convention:
  `src/main.js`.
