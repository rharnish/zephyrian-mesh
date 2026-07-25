# Balloon physics + tamper-evident mesh comms: design/vision

Design notes (thinking, not yet an implementation plan) for two related directions: making
balloon vertical dynamics physically real, and having balloons send **tamper-evident** telemetry
to ground towers through the balloon mesh — with per-hop routing and real cryptography
demonstrated **on demand** for a selected balloon, so the live sim stays cheap. Companion to
`WEATHER_BACKEND_PLAN.md` and `RUST_SIM_PLAN.md`.

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

The vision below adds: (1) realistic vertical dynamics driven by ballast + gas, (2) a command
uplink (satellite/radio), (3) a store-and-forward mesh comms protocol to towers,
(4) tamper-evident message integrity via real crypto with key rotation, and (5) logging +
visualization — with routing and crypto shown on demand for the inspected balloon so cost stays
bounded.

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

## 3. Mesh → tower comms protocol (per-hop routing, shown on demand)

**Data model.** Each balloon periodically emits a **telemetry bundle**: `{origin_id, seq,
created_at, position, alt, gas, ballast, health, ttl}` plus an **environmental sensor block**
`{temperature, pressure, humidity}`. Temperature and pressure come straight from the
`atmosphere.rs` ISA model at the balloon's current altitude (§1 already computes them for the
physics); humidity is synthesized (e.g., a decreasing-with-altitude profile with noise, since
ERA5 humidity isn't in the current wind-only dataset — a natural future tie-in to the "extra
variables" idea in `WEATHER_BACKEND_PLAN.md`). Destination is "any grounded tower."

**Routing — Delay-Tolerant Networking (store-and-forward).** The mesh is intermittent, so bundles
are held in a per-balloon queue and forwarded one hop per opportunity toward the nearest grounded
tower. Forwarding metric: shortest path (BFS/Dijkstra) over the *current* connected component to
the nearest tower node — recomputed as topology changes because edges churn every
`LINK_UPDATE_EVERY_N_TICKS`. When no path exists, hold (store-and-forward); if a bundle ages past
a timeout, **satellite fallback** delivers it directly (reusing the exact policy already in
`connectivity_sweep.rs`).

**Cost control via the on-demand rule.** Running true per-hop routing for all ~2000 balloons
every tick is expensive. So the live server keeps only **cheap aggregate state** per balloon
(pending / delivered / satellite counts, and whether it's currently grounded — already computed).
The **actual hop-by-hop route is computed only when a balloon is selected/queried**: the UI hits
a new `GET /api/balloons/:id/comms`, the server runs one BFS from that balloon to the nearest
grounded tower over the live edge graph, and returns the concrete hop list
(`b_id → b_id → … → t_id`) plus that balloon's message log. This is exactly the
"demonstrate how a system might work" scope — real routing, paid for one balloon at a time.

Protocol specifics worth nailing down: bundle id + dedup (so a bundle isn't double-counted across
paths), hop-count/TTL, per-tower delivery ack, and epidemic-vs-single-path forwarding choice
(single shortest-path recommended for legibility of the visualization).

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
**provably** detectable. Optionally, each forwarding balloon appends a signed "relayed-by"
attestation, giving a verifiable path (detecting a malicious relay that alters a bundle).

**On-demand verification.** The chain hash is cheap and can be maintained continuously;
**Ed25519 signing/verification is done for the selected balloon only** — when queried, the server
(acting as the tower) verifies that balloon's chain + signatures and returns per-record status:
`verified | bad-signature | broken-link`. A UI control can **inject a tamper** into a stored
record to demonstrate the chain flipping red.

Design note: to be fully authentic, signing must happen at record *creation*. To keep creation
cheap at scale, sign only a bounded recent window per balloon, or sign lazily and be explicit that
it's a demonstration. Recommended crates: `ed25519-dalek`, `sha2`.

## 5. Logging & visualization

**Server.** A bounded per-balloon comms event log (created / forwarded / delivered / satellite /
tampered). Lightweight status rides in the existing `Snapshot` (new optional per-balloon fields:
pending/delivered/satellite counts, grounded, `last_channel` (satellite/radio/none), chain-head
hash, verified flag, current key epoch). Rich detail comes from the on-demand
`GET /api/balloons/:id/comms` (route + message log + verification).

**Frontend** (patterns already in the codebase):
- **Select a balloon** (click) → query endpoint → render:
  - **Animated packet** hopping node-to-node along the returned route to a tower, reusing the
    reused-`PolylineCollection`-keyed-by-`pairKey` pattern from `syncLinks` in `src/main.js`
    (a moving billboard/point advancing one edge per interval).
  - A **comms log panel/table** (net-new DOM; the Controls panel is the only precedent): rows of
    `time · seq · hash-prefix · #hops · channel(radio/satellite) · tamper status (green/red)`.
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

1. `atmosphere.rs` + buoyancy physics + ballast/gas state (self-contained; no comms).
2. Per-balloon command uplink (target alt / drop ballast / vent gas) + satellite vs radio channel.
3. Telemetry bundles + aggregate delivery/satellite state in the live server (port the
   `connectivity_sweep` model) + status in `Snapshot`.
4. On-demand `GET /api/balloons/:id/comms`: BFS route + hash chain + Ed25519 verification + key
   rotation.
5. Frontend: selection, animated packet, comms log panel, tamper-demo, command-uplink viz,
   last-delivery glyph.

Each phase is independently reviewable and visually demonstrable.

## Reused existing pieces

- ISA pressure↔altitude math to port into `atmosphere.rs`: `weather-data-server/wind_backend.py`.
- Delivery + satellite-fallback policy: `sim-server/src/bin/connectivity_sweep.rs`.
- Grounded-component / union-find primitives: `sim-server/src/link_detection.rs`,
  `sim-server/src/union_find.rs`.
- Command/REST + Snapshot protocol to extend: `sim-server/src/sim.rs`, `sim-server/src/main.rs`.
- Reused-`PolylineCollection` + reconcile patterns and the Controls-panel-building convention:
  `src/main.js`.
