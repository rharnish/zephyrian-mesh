# Radio vs. satellite connectivity sweep — Rust results (real wind)

**Chart (interactive, split by horizon coeff and sat fallback minutes):** [`results-rust/sweep-chart.html`](results-rust/sweep-chart.html)
**Chart (static, split by horizon coeff [color] and sat fallback minutes [opacity]):** [`results-rust/sweep-chart.png`](results-rust/sweep-chart.png)
**Data:** [`connectivity-sweep-results-rust.csv`](connectivity-sweep-results-rust.csv) (104 rows)
**Generator:** [`connectivity_sweep`](../sim-server/src/bin/connectivity_sweep.rs) (`connectivity_sweep full experiments/sweep-config.json` to reproduce) · charts rendered with [`plot_sweep_html.py`](plot_sweep_html.py) / [`plot_sweep.py`](plot_sweep.py)

## What was measured

A single clean rectangular grid, replacing the previous mixed full-grid +
sub-sweep writeup: horizon coeff **3.4/3.6/3.8/4.0** × balloon count
**50/100/200/300/400/500/600/700/800/900/1000/1200/1600** × sat fallback
minutes **10/60** — 104 combos, every horizon coeff covered at every
balloon count.

Fixed: 30-second continuous-connection ack duration, 5-simulated-minute
payload cadence, 24 simulated hours per combo. This run used the live wind
field from `wind_backend.py` (confirmed via `sim-server.log`: "loaded wind
field from http://127.0.0.1:8000/api/wind-levels", 2026-07-20 23:24 local) —
balloons drift laterally as well as in altitude, closer to the live app's
actual behavior than a zero-wind motion model. Run started 23:24, CSV
finalized 01:03 (2026-07-21).

## Headline finding: a phase transition, resolved at every horizon coeff

![Radio delivery % by balloon count, split by horizon coeff](results-rust/sweep-chart.png)

| Balloon count | Avg. % delivered via radio |
|---|---|
| 50 | 4.9% |
| 100 | 3.8% |
| 200 | 6.4% |
| 300 | 11.8% |
| 400 | 25.6% |
| 500 | 46.6% |
| 600 | 68.2% |
| 700 | 89.3% |
| 800 | 95.4% |
| 900 | 98.4% |
| 1000 | 99.2% |
| 1200 | 99.9% |
| 1600 | 100.0% |

(Averaged over all 4 horizon coeffs × both sat fallback minutes values at
every point — this grid has no gaps, unlike the earlier sub-sweep.) The mesh
climbs in a continuous curve through 400–900, not a cliff: roughly flat and
low (3–12%) through 300 balloons, then a steep rise from 400 to 900,
saturating (>99%) by 1000.

## Horizon coeff shifts the transition, not the shape

| Balloon count | Horizon coeff 3.4 | Horizon coeff 3.6 | Horizon coeff 3.8 | Horizon coeff 4.0 |
|---|---|---|---|---|
| 300 | 6.7% | 8.0% | 10.0% | 22.3% |
| 400 | 9.7% | 16.0% | 30.3% | 46.5% |
| 500 | 19.0% | 34.3% | 62.1% | 71.2% |
| 600 | 42.2% | 69.2% | 69.8% | 91.7% |
| 700 | 75.6% | 87.2% | 96.6% | 97.9% |
| 800 | 89.3% | 93.9% | 98.9% | 99.5% |
| 900 | 95.4% | 98.8% | 99.6% | 99.8% |

Interpolated 50%-crossing points (balloon count at which avg. radio delivery
first reaches 50%):

| Horizon coeff | 50% crossing | 80% crossing |
|---|---|---|
| 3.4 | n≈623 | n≈732 |
| 3.6 | n≈545 | n≈660 |
| 3.8 | n≈462 | n≈638 |
| 4.0 | n≈414 | n≈543 |

Raising the horizon coeff from 3.4 to 4.0 shifts the 50% point left by
about 210 balloons — a real, monotonic effect across all four values, not
just the two endpoints tested previously. The effect is largest in the
500–600 range (up to ~50 points of spread between horizon coeff 3.4 and 4.0)
and collapses at both ends of the sweep, since there's little room for a
horizon coeff effect once a curve is pinned near 0% or 100%.

Averaged across the full grid: **49.4%** (horizon coeff 3.4) → **55.3%**
(3.6) → **60.3%** (3.8) → **65.7%** (4.0). Sat fallback minutes barely
matters on average — **56.6%** (10) vs. **58.7%** (60) — but like horizon
coeff, its effect is concentrated in the transition zone: at 600 balloons,
radio delivery ranges from **29.4%** (horizon coeff 3.4, sat fallback
minutes 10) to **93.5%** (horizon coeff 4.0, sat fallback minutes 60).

## Practical takeaway

**Balloon density remains the dominant lever.** With every horizon coeff now
covered at 100-balloon resolution across the full transition, the curve is a
smooth climb crossing 50% somewhere in the n≈410–620 range depending on
horizon coeff, not a fixed cliff at a single balloon count. Below ~400
balloons, no combination of horizon coeff or sat fallback minutes gets radio
delivery above ~50%; above ~900, all four horizon coeffs are effectively
saturated (>95%) and tuning stops mattering. The 500–700 window is where
parameter choice has the most leverage — tens of points of delivery
difference from horizon coeff alone.

## Limitations

- **1 random seed per combination**, not averaged over repeats — the
  phase-transition pattern (a few percent below the threshold vs. 90%+ above
  it) is well clear of seed-to-seed noise, but individual transition-zone
  cells shouldn't be over-interpreted.
- **Only 2 sat fallback minutes values (10/60) swept**, not the 4-value grid
  (10/20/30/60) from the earlier writeup — this run traded sat fallback
  minutes resolution for full horizon coeff coverage across all 13 balloon
  counts.
- **Ack duration fixed at 30s**, not swept — recomputing links every
  simulated second, unthrottled, is already the dominant compute cost (see
  the [`ConnectivityScratch` optimization](../sim-server/src/link_detection.rs)
  that made a full sweep practical to run at all).
- **Real wind, one snapshot.** The wind field is a single static ERA5 time
  snapshot (see the `wind_backend_perf` note) reused for the full 24
  simulated hours — it's real spatial wind structure, not a real time-varying
  forecast, so "closer to the live app" refers to the motion model, not to
  simulating actual weather evolution.
