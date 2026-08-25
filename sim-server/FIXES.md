# sim-server structure review — findings and fixes

Context: `sim-server` is a line-for-line Rust port of what used to be
the browser-side JS simulation this repo started as (see [README.md](README.md) for the
module-by-module mapping). This doc summarizes a structural review of the
port and tracks what's been fixed vs. still open.

## Already fixed

### Dead/drifted constants in `src/config.js`

`config.rs` and `config.js` were meant to be hand-mirrored ("keep in sync
manually"), but most of the physics/link-detection constants in `config.js`
were no longer read by any JS code — the frontend stopped doing physics once
`sim-server` took over, and `config.js` was never pruned. Confirmed dead via
`grep -rln <NAME> src/*.js` (only `config.js` itself referenced them):

- `TICK_DT_SECONDS`, `TIME_SCALE`, `MAX_VERTICAL_RATE`, `VERTICAL_GAIN`,
  `TARGET_DRIFT_CHANCE_PER_TICK`, `TARGET_DRIFT_RANGE`,
  `LINK_UPDATE_EVERY_N_TICKS`, `GRID_CELL_SIZE_DEG`, `INITIAL_TOWERS`
  (+ its commented-out predecessor)

Worth noting: `TIME_SCALE` in `config.js` was `60`, but `config.rs`'s is
`15.0` — the two had already drifted before either side was dead. Deleting
the dead export removes the wrong value entirely rather than leaving it to
confuse someone later.

**What's still genuinely shared:** `BALLOON_MIN_ALT`, `BALLOON_MAX_ALT`, and
`EARTH_RADIUS`/`EARTH_RADIUS_M` are still read client-side for rendering
(balloon icon scaling, tower range circles, horizon geometry), so those
values do need to keep matching Rust's.

**Fix applied:**
- Removed the 9 dead exports from `src/config.js`.
- Added `src/config.sync.test.js` (vitest): parses
  `sim-server/src/config.rs` as source of truth and asserts the three
  still-shared constants match. Fails loudly, naming the file to check, if
  either side changes without the other or a constant gets renamed.
- Updated the header comment in `sim-server/src/config.rs` to name the three
  constants that still need manual sync and point at the enforcing test.

### String-keyed union-find in `sim.rs`

Node identity in the connectivity graph was `format!("b{}", id)` /
`format!("t{}", id)` — a literal carry-over from JS's natural use of string
map keys. In Rust this was a needless per-node-per-tick allocation, and
nothing stopped a typo'd prefix or an accidental collision between balloon
and tower ids.

**Fix applied:**
- Added `NodeKey` (`Balloon(u32) | Tower(u32)`) to `link_detection.rs` —
  `Copy`, `Ord`, with a `Display`/`wire_pair_key()` conversion to the
  `"b{id}"`/`"t{id}"` wire strings only at the snapshot boundary the JS
  frontend parses (`linkLayer.js`).
- `link_detection::Edge`, `union_find::UnionFind`, and the link-recompute
  block in `sim.rs` now carry `NodeKey` instead of `String` — no more
  per-node `format!`/`.clone()` on the throttled link-recompute path.
- `beacon.rs`'s `parse_key` (now `protocol/dv_dtn/beacon.rs`; it re-parsed the strings `Edge` had just
  formatted) is gone; `MeshAdjacency::rebuild` matches on `NodeKey` directly.
- `bin/mesh_depth.rs` had the same string-keyed adjacency map and was
  updated too (missed by the original review, caught by the build).

### `bundle.rs` mixing delivery logic with stats bookkeeping

At ~1180 lines, `bundle.rs` interleaved the queueing/ack/satellite-fallback
protocol logic with the `BundleStats` counters and histograms it reports
into — reads as a straight port-plus-additions rather than something
reshaped for Rust.

**Fix applied:**
- Moved `BundleStats`, `bump`, and `hist_mean` into a new `bundle_stats.rs`,
  re-exported from `bundle.rs` (`BundleStats`/`hist_mean` still resolve, so
  `sim.rs` and the `bin/protocol_sweep.rs`/`bin/bundle_delivery.rs` callers
  needed no changes).
- At the time of the split `bundle.rs` was ~1050 lines of
  queueing/ack/satellite-fallback logic only and `bundle_stats.rs` ~150 lines
  of counters/histograms. Both have since moved under
  `sim-server/src/protocol/dv_dtn/` and grown with the protocol work — 1500 and
  247 lines today — but the separation still holds.

### `main.rs` request DTOs and `sim::Command` correspondence

Each HTTP handler built a `Command` from its deserialized body inline; the
mapping was implicit and only checked by the compiler catching a type
mismatch, not by anything that documented the pairing.

`Command` itself can't be the wire-format type directly — its
`QueryBalloonComms` variant carries a `tokio::sync::oneshot::Sender`, which
can't derive `Deserialize`, and `RemoveTower`'s `id` comes from a URL
`Path`, not a JSON body — so the per-endpoint `*Body` structs still need to
exist; they were just doing the `Command` mapping inline instead of as a
named conversion.

**Fix applied:**
- Added `impl From<XBody> for Command` next to each of `AddTowerBody`,
  `SetBalloonCountBody`, `SetHorizonCoeffBody`, and `SetPausedBody` in
  `main.rs`. Each handler now sends `body.into()` instead of hand-building
  the `Command` variant inline — the mapping is a named, greppable thing
  instead of implicit handler-body construction.
- `RemoveTower` (id from `Path`) and `QueryBalloonComms` (constructs a
  `oneshot` channel) have no body DTO to convert from, so they're
  unchanged.

## Open — not yet addressed

These were identified during the review but no code changes were made for
them. Flagging for whoever picks this up next.

1. **No schema/version check on the WS snapshot format.** `Snapshot` in
   `sim.rs` serializes with `camelCase` and the JS frontend independently
   agrees on field names by convention; nothing catches a field rename on
   one side until it shows up as a runtime `undefined` in the browser.

## Where to look

- [README.md](README.md) — the JS→Rust module mapping and the three-process
  architecture (`wind_backend.py` / `sim-server` / the browser frontend).
- [src/config.rs](src/config.rs) — constants, now with the sync note.
- [../src/config.js](../src/config.js) and
  [../src/config.sync.test.js](../src/config.sync.test.js) — the JS side and
  its enforcement test.
- [src/sim.rs](src/sim.rs) — the single-task `World` owner; `tick()`'s
  link-recompute block is where the `NodeKey` fix above lives.
- [src/link_detection.rs](src/link_detection.rs) — `NodeKey` and
  `wire_pair_key`.
- [src/protocol/dv_dtn/bundle.rs](src/protocol/dv_dtn/bundle.rs) and
  [src/protocol/dv_dtn/bundle_stats.rs](src/protocol/dv_dtn/bundle_stats.rs) —
  the split above. Both moved under `protocol/` when the comms protocol became
  pluggable; the split itself is unchanged.
- [src/main.rs](src/main.rs) — the `*Body` -> `Command` `From` impls.
