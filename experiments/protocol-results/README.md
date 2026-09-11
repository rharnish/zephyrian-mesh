# protocol-results — charts, and how current each one is

Two experiments write here. Each ships the same figure twice: a **PNG**, which
is what Markdown can embed and what the write-ups link, and an **HTML** twin
with hover values, a legend that isolates one series, and a data table. GitHub
serves `.html` files in a repo as source rather than rendering them, so the
HTML is for GitHub Pages or a local open — the PNG stays canonical.

The two are the same data and the same aggregation (each `*_html.py` imports
its loader and ordering from the PNG script rather than reimplementing them).
For *Truth vs. belief vs. delivery* they also share one palette, validated as a
set in both modes; the PNG's old red "ack lost" line, which sat ΔE 10.4 from the
magenta beside it, is violet in both now. They are still not pixel-identical —
the HTML adds direct hover values and a table — so where a detail differs, the
legend in each file is authoritative for that file.

| Chart | PNG | HTML | Data | Generator |
|---|---|---|---|---|
| Protocol density sweep | [`density-sweep.png`](density-sweep.png) | [`density-sweep.html`](density-sweep.html) | [`../density-sweep-results.csv`](../density-sweep-results.csv) + [`../ground-truth-sweep-results.csv`](../ground-truth-sweep-results.csv) | [`density_sweep.rs`](../../sim-server/src/bin/density_sweep.rs) |
| Truth vs. belief vs. delivery | [`truth-vs-belief-vs-delivery.png`](truth-vs-belief-vs-delivery.png) | [`truth-vs-belief-vs-delivery.html`](truth-vs-belief-vs-delivery.html) | [`../protocol-sweep-results.csv`](../protocol-sweep-results.csv) | [`protocol_sweep.rs`](../../sim-server/src/bin/protocol_sweep.rs) |

`json/` holds `protocol_sweep`'s resumable per-run output (one file per cell and seed). It is the sweep's
scratch space, not a result — the CSV is the result.

## Re-rendering

Rendering is pure: same CSV in, same chart out, so these are safe to re-run
any time to confirm a chart matches its data.

```bash
python3 experiments/plot_density_sweep.py experiments/density-sweep-results.csv \
  --out experiments/protocol-results/density-sweep.png \
  --truth experiments/ground-truth-sweep-results.csv
python3 experiments/plot_density_sweep_html.py experiments/density-sweep-results.csv \
  --out experiments/protocol-results/density-sweep.html \
  --truth experiments/ground-truth-sweep-results.csv

python3 experiments/plot_protocol_sweep.py experiments/protocol-sweep-results.csv \
  --out experiments/protocol-results/truth-vs-belief-vs-delivery.png
python3 experiments/plot_protocol_sweep_html.py experiments/protocol-sweep-results.csv \
  --out experiments/protocol-results/truth-vs-belief-vs-delivery.html
```

## How current the underlying measurements are

The two charts are not equally fresh, and the difference is worth knowing
before either is quoted.

**`density-sweep` is current.** Its CSVs and its generator were produced and
committed together, and no sim code has landed since.

**`truth-vs-belief-vs-delivery` is current too.** It was a July 2026 snapshot
— one run per cell, predating roughly two dozen sim-server commits — until it
was re-run on 2026-09-10 against the shipped dv-dtn defaults. Three things
changed with that re-run, all in `protocol_sweep.rs` and its config:

- **10 seeds per cell**, one CSV row per run; the charts show the mean and a
  ±1 sd band. A single run was not enough: near the percolation threshold two
  runs of the same cell differed by more than 10 points of completion rate.
- **Degree, grounded % and believed % are averaged over the whole run**, not
  read off its final round, to match the delivery counters, which were always
  cumulative over the run.
- **The wind field is named in the config** (`"wind": "1978-06-09T00:00:00"`)
  and read from the wind cache, so a run needs no Python backend and the field a
  CSV was measured against is recorded in the repo. Omit `wind` to fall back to
  fetching whatever `wind_backend.py` serves.

The re-run also changed the chart's shape. With 10 seeds the old single-panel
sawtooth survived, so it was not noise: cells of near-equal degree differ by
15–20 points of completion (longer links reach a tower in fewer hops), and
degree alone does not line them up. The chart is now one panel per balloon
count; see `docs/design/MESH_COMMS_DESIGN.md` §1.1.

```bash
# from the repo root; 150 runs, ~25 min on 4 cores
sim-server/target/release/protocol_sweep full
```

`full` resumes: it skips any run whose JSON is already in this directory or in
`json/`. To re-measure rather than re-combine, clear `json/` first.
