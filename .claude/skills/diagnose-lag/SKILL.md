---
name: diagnose-lag
description: Diagnose cesium-app showing a stale world, or a control snapping back / desyncing from sim-server. The cesium-app-specific commands and history for the general diagnose-stream-lag method.
---

# diagnose-lag (cesium-app)

**Read `diagnose-stream-lag` first** — it has the method (the three queues, the
discriminating readings, why `send()` doesn't mean delivered, state-vs-events).
This file is only what is specific to this project: the commands that work here,
the numbers measured here, and the invariants not to break here.

## The shape of it here

Snapshots are **state**, not events — each is a complete picture of the world,
`edges: None` meaning "keep the last set" being the only exception. So dropping
is always safe, and coalescing is the right answer everywhere.

## Rule 0 is already done

Every snapshot carries `serverTimeMs`, stamped at construction in
`World::tick`. The Controls panel shows it as **View lag**: green under 1s,
amber past that, red past 4s.

Read that before anything else. Under a second and staleness is not your bug.

## Ground truth, without a browser

```bash
node .claude/skills/run-cesium-app/peek.mjs                # all scalar fields
node .claude/skills/run-cesium-app/peek.mjs 's.tick'
node .claude/skills/run-cesium-app/peek.mjs 's.balloons.length'
```

A bare WebSocket, no rendering, always current. If a value is right here, the
server is right — it confirms commands in **30–130ms**, so suspect it last.

## The headless driver lies about timing

`run-cesium-app` runs on software GL and consumes **~0.6 snapshots/s against
the server's 20** — roughly 33x too slow. Use it for "does this render / does
this control fire". Do not use it to measure latency, and do not size a timeout
from it.

## The three readings, with this project's commands

```bash
# 1. sequence gaps — temporary probe in simClient.js's ws.onmessage
#    try { const t = JSON.parse(event.data).tick;
#          (window.__rx = window.__rx || []).push(t); } catch (e) {}

# 2. transport
ss -tnm 'sport = :8080'

# 3. producer memory
P=$(pgrep -f 'target/release/sim-server' | head -1)
awk '/VmRSS/{print $2/1024 " MB"}' /proc/$P/status
```

`World` itself is small; RSS well above that is queued snapshots.

## Worked example (2026-07-28)

Symptom: both Controls sliders bounced back on release; the balloon slider only
when *shrinking*.

| reading | value |
|---|---|
| tick gaps | all exactly 1 — nothing dropped anywhere |
| Send-Q | 3 MB of a 4 MB cap ≈ 1.1s |
| sim-server RSS | **251 MB** ≈ 1400 snapshots ≈ 70s |
| observed lag | 752 ticks (~37s) |

Only the third explained the magnitude: snapshots were queued in the
per-connection WebSocket write buffer. Fixes: `handle_socket` drains to the
newest snapshot before each send (`2c4157b`), and `PendingRequest` in
`src/ui/controlPanel.js` replaced a 600ms cooldown with wait-for-confirmation
(`2605397`).

The asymmetry was the clue that it was queue depth rather than latency: when
shrinking, the stale queued snapshots are the *larger* ones, so the backlog
drains slower.

Payload work from the same session: per-edge endpoint coordinates were 83% of a
snapshot and were overwritten by `refreshPositions` on the same tick. Removing
them took snapshots **725 KB → 183 KB** (`cf2ecf0`).

## Do not break

- **The WebSocket path is browser-only.** The experiment binaries in
  `sim-server/src/bin/` drive `World::tick` in-process and never open a socket,
  so `handle_socket` changes cannot affect headless runs.
- **Adding a `Snapshot` field is safe for them**, verified: none serialize a
  `Snapshot` (both sweeps write their own flat `ResultRow`), none construct one
  literally, and nothing compares results byte-wise — so a non-deterministic
  field can't make a seeded sweep irreproducible. Re-check with:
  ```bash
  grep -n "serde_json::\|to_string_pretty" sim-server/src/bin/*.rs
  ```
- **Keep snapshots self-contained.** It is what lets any of them be dropped and
  what keeps the client unable to drift from the server.

## Restarting after a server change

`cargo build --release` does not affect the running process, and a new
sim-server that finds `:8080` occupied **panics and exits silently**, leaving
you on the old binary wondering why nothing changed.

```bash
cd sim-server && cargo build --release && cargo test
pkill -f 'target/release/sim-server'; sleep 2
./target/release/sim-server
ss -ltnp | grep ':8080'      # confirm the PID actually changed
```
