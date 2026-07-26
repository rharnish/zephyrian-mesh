# Balloon physics: design/vision

Design notes (thinking, not yet an implementation plan) for making balloon vertical dynamics
physically real — buoyancy driven by ballast and lift gas, with a command uplink that acts through
those finite actuators. Companion to `MESH_COMMS_DESIGN.md` (the comms half, which is independent
of this one — see "Relationship to the comms track" below), `WEATHER_BACKEND_PLAN.md`, and
`RUST_SIM_PLAN.md`.

## Where things stand today

The simulator models balloons minimally: **pure wind advection** horizontally and a
**proportional "thermostat"** vertically that drives altitude toward a randomly-drifting target
(`sim-server/src/balloon.rs`). There is no mass, gas, ballast, buoyancy force, or per-balloon
command input; only `id/lon/lat/alt` reach the frontend.

The vision below adds: (1) realistic vertical dynamics driven by ballast + gas, and (2) a command
uplink (satellite/radio) that drives those actuators.

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
(key rotation — see §2 of `MESH_COMMS_DESIGN.md`).

Two delivery channels, which the sim distinguishes (and the UI visualizes):
- **Satellite**: always reachable (optionally with a latency), can command any balloon anytime.
- **Radio**: only when the balloon is currently reachable from a tower through the mesh (its
  component is `grounded`). The command rides *down* the mesh — the reverse of telemetry going up.

Each executed command records which channel delivered it, so the UI can show a satellite downlink
beam vs. a highlighted radio path through the mesh.

## Suggested phasing

- **P1.** `atmosphere.rs` + buoyancy physics + ballast/gas state (self-contained; no comms).
- **P2.** Per-balloon command uplink (target alt / drop ballast / vent gas) + satellite vs radio
  channel.

## Relationship to the comms track

**The comms track does not depend on this one, and this one does not depend on it.** The only
coupling runs comms → physics, in two places, and `MESH_COMMS_DESIGN.md` names both:

- Its telemetry bundle carries an environmental sensor block whose temperature and pressure come
  from the `atmosphere.rs` ISA model in §1 above. Until P1 exists these can be stubbed and swapped
  later.
- Its key-rotation design uses the `RotateKey{id}` uplink command defined in §2 above.

So the two tracks can be done in either order, or interleaved. Nothing in this document is blocked
on comms.

## Reused existing pieces

- ISA pressure↔altitude math to port into `atmosphere.rs`: `weather-data-server/wind_backend.py`.
- Command/REST + Snapshot protocol to extend: `sim-server/src/sim.rs`, `sim-server/src/main.rs`.
- Per-balloon billboard / altitude-glyph rendering to extend: `src/main.js`.
