---
name: diagnose-lag
description: Find out why the cesium-app browser is showing a stale world, or why a control snaps back / lags / desyncs from sim-server. Locates where a snapshot backlog is accumulating (client render, kernel socket, or the server's own write buffer) before changing anything.
---

# diagnose-lag

For symptoms that look like the UI disagreeing with the server: a slider that
bounces back on release, a readout that trails, a globe that is subtly behind,
a control that ignores another tab.

Nearly all of these are one root cause — **the browser is rendering a world
older than the one the server has** — and the whole job is finding *where* the
backlog sits before trying to fix it. The three candidates look identical from
the UI and need completely different fixes.

## Rule 0: read the lag before theorising

Every snapshot carries `serverTimeMs`, stamped when the server *built* it. The
Controls panel shows the result as **View lag**.

- **< 1s** — healthy. Whatever you are chasing is not staleness; stop here.
- **seconds, stable** — the client is saturated but keeping pace.
- **seconds, climbing** — a backlog is accumulating. Continue below.

This exists because it used to take a custom probe plus a second WebSocket to
answer. Don't rebuild that; read the number.

## Rule 1: the server is almost never the problem

Measure before suspecting it. It confirms a command in **30–130ms**:

```bash
node .claude/skills/run-cesium-app/peek.mjs 's.tick'          # current server tick
node .claude/skills/run-cesium-app/peek.mjs                   # all scalar fields
```

`peek.mjs` is a bare WebSocket with no rendering, so it is always current. If a
value is right there, the server is right and the disagreement is downstream.

## Rule 2: headless Chromium cannot be trusted for timing

`run-cesium-app` runs on software GL and consumes snapshots **~33x slower than
the server produces them** (measured: 0.6/s against 20/s). It is excellent for
"does this render / does this control fire" and actively misleading for "how
fast is this". Reproduce timing symptoms in a real browser, or accept that
headless is a worst case and size fixes accordingly.

## Locating the backlog

Run all three. Only the combination discriminates.

### 1. Tick gaps — is anything being dropped?

Record the tick of every message *before* any client-side coalescing:

```js
// temporary probe in simClient.js's ws.onmessage
try { const t = JSON.parse(event.data).tick;
      (window.__rx = window.__rx || []).push(t); } catch (e) {}
```

Then read the gaps between consecutive entries:

- **all gaps == 1** — nothing is being dropped anywhere. The client is being
  fed an unbroken FIFO, so a queue is holding *every* snapshot. Go to 2 and 3.
- **large / irregular gaps** — frames are being skipped, which is the healthy
  behaviour under load. Staleness is then bounded and is not your bug.

### 2. Kernel socket buffer — how much is in flight?

```bash
ss -tnm 'sport = :8080'      # Send-Q column, and tb= is the buffer cap
```

Compare against snapshot size to convert bytes to seconds:

```bash
node .claude/skills/run-cesium-app/peek.mjs 's.balloons.length + " balloons, " + (s.edges ? s.edges.length : "no") + " edges"'
```

A ~4MB buffer at ~183KB/snapshot is ~22 snapshots ≈ 1.1s. If Send-Q is near its
cap but the observed lag is far larger, the socket is **not** where the backlog
is.

### 3. Server memory — the one that is easy to miss

```bash
P=$(pgrep -f 'target/release/sim-server' | head -1)
awk '/VmRSS/{print $2/1024 " MB"}' /proc/$P/status
```

`World` itself is small. Resident memory far above that is queued snapshots in
the per-connection WebSocket write buffer, because `socket.send(..).await`
returns when the sink accepts a message, not when the browser receives it.

**Worked example (2026-07-28):** tick gaps all 1, Send-Q 3MB of a 4MB cap
(~1.1s), RSS **251MB** (~1400 snapshots ≈ 70s), observed lag 752 ticks (~37s).
Only the third reading explained the magnitude. Fix was `handle_socket`
draining to the newest snapshot before each send (commit 2c4157b).

## Fixes, by where the backlog actually is

| Location | Symptom | Fix |
|---|---|---|
| Server write buffer | RSS climbs with a slow client attached; tick gaps all 1 | Drain to newest before send (already done — `handle_socket`) |
| Payload size | Lag scales with balloon/edge count | Stop sending redundant data. Precedent: dropping per-edge coordinates cut snapshots 725KB → 183KB, since `pairKey` plus the balloons array already carried it |
| Client render cost | RSS flat, Send-Q flat, browser still behind | Coalesce in `simClient.js` (already done), then reduce per-snapshot render work |

## Controls that disagree with the server

A control that snaps back is a *symptom* of the above, not a separate bug: it
applied a snapshot older than its own change.

Never fix this with a timer. Wall-clock time on the client says nothing about
*which* snapshot is being applied — that was the original 600ms cooldown, and
it failed exactly when it mattered. Use `PendingRequest` in
`src/ui/controlPanel.js`: after requesting a value, ignore that control's
snapshot value until a snapshot actually carries what was asked for. Correct at
any backlog depth.

## Do not break

- **Snapshots must stay complete and self-contained.** That property is what
  lets any of them be dropped, and what keeps the client unable to *drift* from
  the server (worst case is stale, never wrong). A delta protocol would trade
  it away and make every message load-bearing — strictly worse for this
  failure mode. See the reasoning in `simClient.js`.
- **The WebSocket path is browser-only.** The experiment binaries in
  `sim-server/src/bin/` drive `World::tick` in-process and never open a socket,
  so `handle_socket` changes cannot affect headless runs. Adding a `Snapshot`
  field is safe for them too: none serialize a `Snapshot`, none construct one
  literally, and nothing compares results byte-wise. Re-check with:
  ```bash
  grep -n "serde_json::\|to_string_pretty" sim-server/src/bin/*.rs
  ```
