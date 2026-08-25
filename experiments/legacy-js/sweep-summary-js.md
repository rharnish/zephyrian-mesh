# Radio vs. satellite connectivity sweep — summary

**Chart:** [`results/sweep-chart.html`](results/sweep-chart.html)
**Data:** [`connectivity-sweep-results.csv`](connectivity-sweep-results.csv) (96 rows)
**Generator:** [`connectivity-sweep.mjs`](connectivity-sweep.mjs) (`experiments/legacy-js/run-shards.sh` to reproduce)

## What was measured

% of balloon data payloads that reached the ground via radio vs. satellite fallback,
swept across:

- **Horizon coefficient:** 3.4, 3.6, 3.8, 4.0
- **Balloon count:** 50, 100, 200, 400, 800, 1600
- **Satellite-fallback timeout:** 10, 20, 30, 60 minutes

Fixed: a payload counts as radio-delivered once a balloon has held a **continuous
30-second** connection to a grounded cluster (ack duration, taken out of the sweep
per Roy). Payloads generate every 5 simulated minutes per balloon; a payload not
radio-delivered before its timeout falls back to satellite (always succeeds).
96 combinations, 24 simulated hours each, headless (no wind — see Limitations).

## Headline finding: a sharp connectivity phase transition around 400-800 balloons

| Balloon count | Avg. % delivered via radio (across all coeffs/timeouts) |
|---|---|
| 50 | 3.4% |
| 100 | 3.3% |
| 200 | 5.0% |
| 400 | 22.7% |
| 800 | 94.1% |
| 1600 | 100.0% |

Below ~400 balloons, the mesh is too sparse to reliably chain a balloon back to a
tower — the overwhelming majority of payloads fall back to satellite regardless of
horizon coefficient or timeout. Above ~800, the mesh is dense enough that nearly
every payload finds a radio path, again almost regardless of the other parameters.
This is a classic **percolation threshold**: connectivity isn't a smooth dial, it's
a density the network either has or doesn't.

## Horizon coefficient and timeout matter — but only in the transition zone

- **Horizon coefficient** has a real but modest effect on average (34.2% at
  coeff 3.4 → 40.9% at coeff 4.0), far smaller than the balloon-count effect.
- **Satellite-fallback timeout barely matters at all** on average (37.8% at
  10 min vs. 38.4% at 60 min) — below the density threshold, more waiting time
  rarely helps (the connection never sustains 30s regardless); above it, payloads
  succeed almost immediately, so extra time is moot either way.
- Both parameters matter most **inside the transition zone**: at exactly 400
  balloons, radio delivery ranges from 6.3% to 47.6% depending on coefficient and
  timeout — this is the one region where tuning those knobs actually changes the
  outcome.

## Practical takeaway

If the goal is reliable radio delivery, **balloon density is the lever that
matters** — pushing past ~800 balloons (in this topology, with these 12 towers)
gets you to ~95-100% radio delivery almost independent of horizon coefficient or
timeout tuning. Below that density, no amount of horizon-coefficient or timeout
tuning fully compensates — satellite fallback will carry the majority of traffic.

## Limitations (read before trusting absolute numbers)

- **Zero wind.** The real wind backend wasn't available in this environment (see
  the `wind_backend_perf` memory), so balloons hold their spawn longitude/latitude
  for the entire run — only altitude drifts. Real wind-driven lateral movement
  would let balloons drift into and out of range of each other and of towers,
  which could meaningfully change the transition point. Relative comparisons
  across the swept parameters should still be directionally meaningful; the exact
  balloon-count threshold should not be taken as a real-world number.
- **1 random seed per combination**, not averaged over repeats (chosen deliberately
  to keep the sweep to hours instead of days). Individual cells have some
  seed-to-seed noise; the phase-transition pattern is large enough (3-5% below
  the threshold vs. 90%+ above it) to be well clear of that noise, but small
  differences between adjacent cells (e.g. two horizon coefficients at the same
  balloon count) shouldn't be over-interpreted.
- **Ack duration fixed at 30s**, not swept. An earlier attempt at sweeping it too
  (with a cadence-throttled connectivity check) produced a bug where longer ack
  windows looked artificially *more* reliable — fixed by recomputing links every
  simulated second unconditionally, at higher compute cost, which is part of why
  ack duration was dropped from this sweep rather than re-included at the higher
  resolution needed to trust it.
