# legacy-js — archived JS connectivity sweep

Archived, not under active development. The app's simulation (balloon
motion, link detection) moved entirely to `sim-server` (Rust) — see
`../../RUST_SIM_PLAN.md` and `../../sim-server/README.md` — so this JS
implementation of the connectivity sweep, and the `src/*.js` files it alone
still depended on (`balloon.js`, `linkDetection.js`, `spatialGrid.js`,
`unionFind.js`, now under [`src/`](src/) here), were moved out of the live
app tree and into this folder rather than deleted, so the original JS
sweep stays runnable for reference/comparison against the Rust version.

Kept for:
- Reproducing `connectivity-sweep-results.csv` / `sweep-summary-js.md`
  (zero-wind, non-reproducible seeds — see `../README.md` for the full
  JS-vs-Rust comparison table).
- Cross-checking a new Rust change against the original implementation if
  the two ever appear to disagree.

Still shares `../../src/geo.js`, `towerModel.js`, `windField.js`, and
`config.js` with the live app (those aren't dead code — they're used for
client-side rendering) — only the four files now under `src/` here were
exclusively used by this sweep and had no other purpose left in `src/`.

Run it the same way as before, just from its new path:

```bash
node experiments/legacy-js/connectivity-sweep.mjs bench
node experiments/legacy-js/connectivity-sweep.mjs quick
./experiments/legacy-js/run-shards.sh
```
