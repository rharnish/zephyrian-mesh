# Radio vs. satellite connectivity sweep — Rust results (real wind)

**Chart:** https://claude.ai/code/artifact/eaaf95b3-da60-452e-a041-686ed3c1be38
**Data:** [`connectivity-sweep-results-rust.csv`](connectivity-sweep-results-rust.csv) (96 rows)
**Generator:** [`connectivity_sweep`](../sim-server/src/bin/connectivity_sweep.rs) (`experiments/run-shards-rust.sh` to reproduce)

## What was measured

Same model and sweep grid as the original JS run ([`sweep-summary.md`](sweep-summary.md)),
re-run on the Rust port:

- **Horizon coefficient:** 3.4, 3.6, 3.8, 4.0
- **Balloon count:** 50, 100, 200, 400, 800, 1600
- **Satellite-fallback timeout:** 10, 20, 30, 60 minutes

Fixed: 30-second continuous-connection ack duration, 5-simulated-minute payload
cadence, 96 combinations, 24 simulated hours each. **Unlike the original JS
sweep, this run used the live wind field from wind_backend.py** (confirmed in
every shard log) — balloons drift laterally as well as in altitude, closer to
the live app's actual behavior than the zero-wind JS baseline.

## Headline finding: the same phase transition, slightly sharper

| Balloon count | Avg. % delivered via radio (across all coeffs/timeouts) |
|---|---|
| 50 | 3.6% |
| 100 | 3.2% |
| 200 | 6.8% |
| 400 | 25.7% |
| 800 | 95.6% |
| 1600 | 100.0% |

Same percolation-threshold shape as the zero-wind JS sweep: below ~400 balloons
the mesh essentially never chains back to a tower; above ~800 it almost always
does. The transition midpoint (400 balloons) delivers a few points higher on
average with real wind (25.7% vs. 22.7%) — lateral drift gives balloons more
chances to pass through range of a tower or another connected balloon over 24
hours than holding a fixed lon/lat does, though the effect is modest next to
the balloon-count effect itself.

## Horizon coefficient and timeout — same pattern as before

- **Horizon coefficient**: 34.8% (coeff 3.4) → 44.4% (coeff 4.0) average — a
  real effect, same direction and similar magnitude to the JS run (34.2% →
  40.9%).
- **Satellite-fallback timeout** still barely matters on average: 38.6% (10
  min) vs. 40.1% (60 min).
- The transition zone is still where parameter choice matters most: at 400
  balloons, radio delivery ranges from **5.9%** (coeff 3.6, 20-min timeout) to
  **57.9%** (coeff 4.0, 30-min timeout) — an even wider spread than the JS
  run's 6.3–47.6%, consistent with wind-driven drift adding variance exactly
  where the network is balanced on the edge of connectivity.

## Practical takeaway

Same conclusion as the JS sweep, now confirmed under wind-driven motion:
**balloon density is the lever that matters**. Pushing past ~800 balloons
gets to ~95-100% radio delivery almost independent of horizon coefficient or
timeout tuning; below that density, no amount of parameter tuning fully
compensates. Real wind doesn't change the shape of the result — it nudges the
transition-zone numbers up a little and widens their spread, which is what
you'd expect from adding a source of lateral movement to a network that's
already right at its percolation threshold.

## Limitations

- **1 random seed per combination**, not averaged over repeats — same caveat
  as the JS run; the phase-transition pattern (a few percent below the
  threshold vs. 90%+ above it) is well clear of seed-to-seed noise, but
  individual transition-zone cells shouldn't be over-interpreted.
- **Ack duration fixed at 30s**, not swept, for the same reason noted in the
  JS summary (recomputing links every simulated second, unthrottled, is
  already the dominant compute cost — see the [`ConnectivityScratch`
  optimization](../sim-server/src/link_detection.rs) that made a full 96-combo
  sweep practical to run at all).
- **Real wind, one snapshot.** The wind field is a single static ERA5 time
  snapshot (see the `wind_backend_perf` note) reused for the full 24
  simulated hours — it's real spatial wind structure, not a real time-varying
  forecast, so "closer to the live app" refers to the motion model, not to
  simulating actual weather evolution.
