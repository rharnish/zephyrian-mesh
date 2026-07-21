---
name: sweep-summary
description: Turn a connectivity-sweep results CSV (JS or Rust connectivity_sweep output) into a sweep-summary-*.md writeup and chart, matching the existing experiments/sweep-summary*.md convention.
---

# sweep-summary

Workflow for writing up a connectivity-sweep run (`connectivity_sweep full`
for Rust — the live implementation; `experiments/legacy-js/run-shards.sh`
for the archived JS version — see `experiments/README.md`) once its CSV
exists. Three steps: compute, write, chart. Only the first step is
mechanical — the other two are judgment calls this skill does not automate
away.

## 1. Compute — `experiments/summarize_sweep.py`

```bash
python3 experiments/summarize_sweep.py experiments/connectivity-sweep-results-<variant>.csv \
  --chart-json experiments/results-<variant>/chart-data.json
```

Prints markdown tables to stdout (avg % radio-delivered by balloon count, by
horizon coefficient, by timeout, and a transition-zone detail table for
whichever balloon count has the widest spread — auto-detected, not
hardcoded to 400). Writes `chart-data.json` with the series data the chart
needs: `{"xLabels": [...], "series": {"<coeff>": [avg per xLabel, ...]}}`.

Don't hand-compute these numbers — this script exists because doing it by
hand is slow and error-prone at 96 rows; always regenerate rather than
eyeballing the CSV.

## 2. Write — `experiments/sweep-summary-<variant>.md`

Follow the structure of the existing summaries
([`sweep-summary-js.md`](../../experiments/legacy-js/sweep-summary-js.md),
[`sweep-summary-rust.md`](../../experiments/sweep-summary-rust.md)):
header with Chart/Data/Generator links, "What was measured", a headline
finding, secondary-parameter effects, a practical takeaway, and a
Limitations section. Use the script's tables verbatim for the numbers; write
the prose — the interpretation (is this a phase transition? how does it
compare to the prior run? what's the practical implication?) and the
Limitations section (seed count, fixed params, data provenance — e.g.
zero-wind vs. real-wind, single static snapshot vs. time-varying) require
knowing the run's context, not just its numbers, so don't try to template
them.

## 3. Chart — `experiments/results-<variant>/sweep-chart.html`

Build via the `dataviz` skill (line chart, one series per horizon
coefficient, x = balloon count, y = % radio). Feed it `chart-data.json`'s
`series`/`xLabels` rather than recomputing from the CSV. Publish as an
Artifact for the chat link, **and** save the same self-contained HTML file
locally at `experiments/results-<variant>/sweep-chart.html` — that's the
established convention (see `experiments/legacy-js/results/sweep-chart.html`
from the original JS run), so the chart survives independent of any
artifact URL.

That path is covered by the `experiments/results*` gitignore rule (it's a
results directory), so committing it needs `git add -f`.

## Gotchas

- The transition zone isn't always 400 balloons — it moves depending on
  topology/wind/parameters. Trust the script's auto-detected `n_best`, don't
  assume it matches a previous run.
- `results-rust-no-wind/` and `results-rust/` are different runs (see
  `connectivity_sweep.rs`'s wind-fetch fallback) — check the run's console
  output (or `shard*.log` for older JS shard-script runs) for "Loaded real
  wind field" vs. "using zero wind" before writing prose that claims one or
  the other; don't infer it from the directory name alone.
