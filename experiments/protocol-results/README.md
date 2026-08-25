# protocol-results — charts, and how current each one is

Two experiments write here. Each ships the same figure twice: a **PNG**, which
is what Markdown can embed and what the write-ups link, and an **HTML** twin
with hover values, a legend that isolates one series, and a data table. GitHub
serves `.html` files in a repo as source rather than rendering them, so the
HTML is for GitHub Pages or a local open — the PNG stays canonical.

The two are the same data and the same aggregation (each `*_html.py` imports
its loader and ordering from the PNG script rather than reimplementing them),
but they are **not pixel-identical**: in *Truth vs. belief vs. delivery* the
unacked series is red in the PNG and violet in the HTML. The PNG's pink/red
pair measures ΔE 10.4 against a separation floor of 15, so the HTML twin moves
it; the PNG is kept as-is because the write-ups already embed it. Where the two
disagree on a colour, the legend in each is authoritative for that file.

| Chart | PNG | HTML | Data | Generator |
|---|---|---|---|---|
| Protocol density sweep | [`density-sweep.png`](density-sweep.png) | [`density-sweep.html`](density-sweep.html) | [`../density-sweep-results.csv`](../density-sweep-results.csv) + [`../ground-truth-sweep-results.csv`](../ground-truth-sweep-results.csv) | [`density_sweep.rs`](../../sim-server/src/bin/density_sweep.rs) |
| Truth vs. belief vs. delivery | [`truth-vs-belief-vs-delivery.png`](truth-vs-belief-vs-delivery.png) | [`truth-vs-belief-vs-delivery.html`](truth-vs-belief-vs-delivery.html) | [`../protocol-sweep-results.csv`](../protocol-sweep-results.csv) | [`protocol_sweep.rs`](../../sim-server/src/bin/protocol_sweep.rs) |

`json/` holds `protocol_sweep`'s resumable per-combo output. It is the sweep's
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

**`truth-vs-belief-vs-delivery` is a July 2026 snapshot.** Its CSV predates
roughly two dozen sim-server commits — among them the `MeshProtocol` trait
split, `DvDtnParams`, giving the protocol its own RNG stream separate from the
world's, and making link detection reproducible. Re-running the same grid on
current code against the same wind file reproduces the *shape* (the percolation
knee, truth above belief above delivery) but not the numbers: completion rate
moves by up to ~12 points at some combos, and mean degree by a couple of
percent at the dense end and more at the sparse end, where a handful of links
is the whole measurement. A protocol RNG stream that no longer shares the
world's is enough on its own to move every figure, so this is expected drift
rather than a regression — and it holds regardless of whether that re-run's
wind field exactly matched the original's, which (see below) cannot be
established from the repo. Read the chart for its argument, not for its
values, until it is re-run.

**Re-running `protocol_sweep` is not a one-liner**, which is why the CSV has
been left alone. Unlike the sweeps that take `--wind`, it fetches its wind
field live from `wind_backend.py` on `:8000` and has no cache path, so a run
needs the Python backend up and a NetCDF file in `weather-data-server/data/`.
Which file it serves is set by `weather-data-server/wind_source.json`, which is
**not tracked** — so the wind a given CSV was measured against is not recorded
anywhere in the repo, and a re-run reproduces the old numbers only if that file
still points where it did at the time. Teaching `protocol_sweep` to take
`--wind` like its siblings would close both gaps at once.
