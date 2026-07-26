# Timing model: ticks, rounds, and seconds

Everything in this simulator is scheduled in **ticks**, which is convenient for the code and
useless for thinking. This document converts the whole timing model into seconds and names the
levers that change it.

`sim-server/src/config.rs` is the source of truth, and every number below is *derived* from the
constants there. Rather than trusting this file, print the current values:

```bash
cargo run --release --bin timing
```

That binary computes all of it from the live constants, so it cannot go stale. **If it disagrees
with this document, it is right and this document needs updating.**

## Three clocks

```
1 tick        = 50 ms real          = 60 s simulated      ← the heartbeat
1 link round  = 3 ticks   = 0.15 s real  = 3 sim min
1 comms round = 8 ticks   = 0.40 s real  = 8 sim min
```

Everything follows from the first line: the simulation runs at **1200× real time**.

- 1 real second = 20 ticks = **20 simulated minutes**
- 1 real minute = **20 simulated hours**

The three clocks exist because they answer different questions. **Real time** is what you watch.
**Simulated time** is what the physics believes, and the only clock in which "a 40-minute radio
duty cycle" is a meaningful claim. **Comms rounds** are what the protocol counts, and exist so the
protocol's pace can be tuned without disturbing either of the other two — see §1.2 of
`MESH_COMMS_DESIGN.md` for why that separation was necessary.

## Everything in seconds

| Parameter | Config constant | **Real seconds** | Simulated time |
|---|---|---|---|
| Snapshot / frame | `TICK_INTERVAL_MS = 50` | 0.05 s | 1 min |
| Link recompute | `LINK_UPDATE_EVERY_N_TICKS = 3` | 0.15 s | 3 min |
| Comms round | `COMMS_EVERY_N_TICKS = 8` | 0.40 s | 8 min |
| Beacon transmission | `BEACON_INTERVAL_ROUNDS = 5` | **2.0 s** | 40 min |
| Beacon jitter | `BEACON_JITTER_ROUNDS = 1` | ±0.4 s | ±8 min |
| Belief expiry | `BELIEF_MAX_AGE_ROUNDS = 60` | **24 s** | 8 hours |
| Full discovery (measured, 1200 balloons) | ~50 rounds | **20 s** | ~6.7 hours |

In plain terms: **a balloon speaks every 2 seconds; a belief it cannot refresh dies after 24
seconds; a beacon takes about 20 seconds to cross the planet.**

`BEACON_MAX_HOPS = 20` is deliberately absent — it is a hop budget, not a duration.

## Doing the arithmetic

Two rules cover nearly every question:

```
real seconds   = rounds × 0.4
simulated mins = rounds × 8
```

The general forms, if `COMMS_EVERY_N_TICKS` changes:

```
real seconds   = rounds × COMMS_EVERY_N_TICKS × TICK_INTERVAL_MS / 1000
simulated secs = rounds × COMMS_EVERY_N_TICKS × TICK_DT_SECONDS × TIME_SCALE
```

## The three levers

They are not interchangeable, and each has a distinct cost.

| Lever | Real time | Simulated time | Cost |
|---|---|---|---|
| `COMMS_EVERY_N_TICKS` | scales | scales | none directly — but simulated durations drift from plausible |
| `TIME_SCALE` | — | scales only | balloons visibly drift slower |
| `TICK_INTERVAL_MS` | scales only | — | breaks render smoothness |

**`COMMS_EVERY_N_TICKS` is the pacing dial.** It moves real and simulated time together, which is
why raising it to 8 bought watchable pacing but pushed belief expiry out to a somewhat implausible
8 simulated hours.

**To slow the protocol on screen while holding simulated durations fixed**, raise
`COMMS_EVERY_N_TICKS` and lower `TIME_SCALE` by the same factor. The price is balloons drifting
proportionally slower.

**Leave `TICK_INTERVAL_MS` alone.** It is the snapshot rate, and the client sets balloon positions
directly from each snapshot with no interpolation, so slowing it makes the globe stutter. Adding
client-side interpolation would free this lever up — a real option, but not a small one.

**Do not touch `BELIEF_MAX_AGE_ROUNDS` for pacing.** It is pinned from below by measured
convergence: a belief must survive long enough to cross the mesh, or deep balloons expire it on
arrival and can never hold a route. See §1.2 of `MESH_COMMS_DESIGN.md`.

## Tuning reference

| `COMMS_EVERY_N_TICKS` | Beacon every | Belief dies after | Full discovery |
|---|---|---|---|
| 4 | 1.0 s | 12 s | 10 s |
| **8** (current) | 2.0 s | 24 s | **20 s** |
| 12 | 3.0 s | 36 s | 30 s |
| 16 | 4.0 s | 48 s | 40 s |

`cargo run --release --bin timing` prints this table with a wider range of candidates, plus a
**simulated** belief-age column — the plausibility check. A duty-cycled HAB radio trusting a route
belief for 8 simulated hours is already generous; at `COMMS_EVERY_N_TICKS = 24` it is a full day.
When real-time pacing and simulated plausibility pull apart, that is the signal to lower
`TIME_SCALE` rather than push the comms clock further.

After changing it, confirm the protocol still behaves with
`cargo run --release --bin beacon_convergence`, which prints its own round → real-seconds
conversion and asserts beliefs drain completely.

## Why this matters for design decisions

Timing questions that look weighty in simulated minutes can be invisible in real seconds. The
open C2 question is the standing example: does a telemetry bundle advance one hop per **tick**
(0.05 s) or one hop per **duty-cycle slot** (2 s)? Across a 14-hop path that is **0.7 seconds
versus 28** — a decision worth making deliberately, and one that was not answerable at all before
the comms clock was separated out, when the two options were 0.7 s and 3.5 s.

Before arguing about a timing constant, convert it to seconds and ask whether the difference is
something a person could see.
