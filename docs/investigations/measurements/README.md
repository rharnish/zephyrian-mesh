# Raw measurements — C2 bundle delivery investigation

Console output backing `../bundle-delivery-report.html`. Every figure in that
report is rendered from numbers transcribed out of these files.

| File | What it is |
|---|---|
| `baseline.txt` | Density sweep, shipped protocol. 1200 balloons, 2000 rounds/coefficient. |
| `nearest.txt` | Same sweep with the `prefer_nearer` ablation (`PREFER_NEARER=1`). |
| `baseline_4000_single.txt` | Longer 4000-round run at the default density; the headline stall/hop breakdown. |
| `demand_sweep.txt` | `bundle_interval_rounds` swept 50→1600 at coeff 4.12. **The decisive result.** |
| `ceiling_by_density.txt` | Tower-adjacent population and last-hop ceiling at coeffs 3.0 / 3.57 / 5.0. |

Reproduce with:

```bash
cargo run --release --bin bundle_delivery [n] [rounds] [coeff]
PREFER_NEARER=1 cargo run --release --bin bundle_delivery 1200 2000
cargo run --release --bin mesh_depth
```

All runs are single-seed. Completion at the default density varies 48–56% between
runs and the tower-adjacent count varies 19–29 at identical configuration, so treat
individual figures as approximate — the robust result is that delivery throughput is
flat against offered load.

A first attempt at the queue sweep was discarded rather than reported: it edited
`config.rs` concurrently with the demand sweep doing the same, cross-contaminating
both. It was re-run in isolation afterwards — see `queue_sweep.txt` below. **Any
harness script that edits `config.rs` must run against an isolated copy of the
source tree**, which is what the later sweeps do.

## Added after the contact-window change

| File | What it is |
|---|---|
| `contact_sweep.txt` | `batch.tower_contact` swept 1→8. Window 1 reproduces the old one-bundle-per-duty-cycle rule, so row 1 is the "before". |
| `queue_sweep.txt` | `relay_queue_capacity` swept 1→16 **under** `batch.tower_contact = 4`. |

**Reading `queue_sweep.txt`:** use the `blocked` figure, not `completion`. Completion is
non-monotone there because it is dominated by how many balloons were in tower range that run
(15.0–25.8 across the five runs, correlating with delivered/round at r = 0.94). Blocking is the
unconfounded, monotone signal. Any future single-seed sweep should normalise by the
tower-adjacent population for the same reason.

A live cross-check against the running server (1200 balloons, coeff 4.12, zero wind) measured
**3.61 delivered/round** versus the harness's 3.82 — within 6%, so the offline harness does model
the shipping server.
