#!/usr/bin/env python3
"""
Aggregates a connectivity-sweep results CSV (connectivity-sweep.mjs or
connectivity_sweep.rs output — same column layout for both) into:

  1. Markdown tables (avg % radio-delivered by balloon count, by horizon
     coefficient, by fallback timeout, plus a transition-zone detail table
     for whichever balloon count has the widest spread) — printed to stdout,
     ready to paste into a sweep-summary-*.md.
  2. A JSON blob of chart series data (avg pctRadio by horizonCoeff x
     nBalloons) shaped for the sweep-chart.html chart script's `data` object
     — written alongside the CSV.

This script only computes; it does not write prose. The narrative sections
of a sweep-summary-*.md (interpretation, comparison to prior runs,
limitations) still need a human/LLM pass over this output — see the
sweep-summary skill.

Usage:
    python3 experiments/summarize_sweep.py experiments/connectivity-sweep-results-rust.csv
    python3 experiments/summarize_sweep.py <csv> --chart-json experiments/results-rust/chart-data.json
"""

import argparse
import csv
import json
import sys
from collections import defaultdict
from statistics import mean


def load_rows(csv_path):
    rows = []
    with open(csv_path, newline="") as f:
        for r in csv.DictReader(f):
            r["horizonCoeff"] = float(r["horizonCoeff"])
            r["nBalloons"] = int(r["nBalloons"])
            r["fallbackTimeoutMin"] = int(r["fallbackTimeoutMin"])
            r["pctRadio"] = float(r["pctRadio"])
            r["pctSatellite"] = float(r["pctSatellite"])
            rows.append(r)
    return rows


def avg_by(rows, key):
    groups = defaultdict(list)
    for r in rows:
        groups[r[key]].append(r["pctRadio"])
    return {k: mean(v) for k, v in sorted(groups.items())}


def markdown_table(headers, rows):
    lines = ["| " + " | ".join(headers) + " |", "|" + "|".join(["---"] * len(headers)) + "|"]
    for row in rows:
        lines.append("| " + " | ".join(str(c) for c in row) + " |")
    return "\n".join(lines)


def transition_zone(rows):
    """The nBalloons value with the widest spread of pctRadio across the
    other swept params — the region where horizon coeff / timeout actually
    change the outcome, as opposed to the saturated low/high ends."""
    by_n = defaultdict(list)
    for r in rows:
        by_n[r["nBalloons"]].append(r)
    n_best, spread_best = None, -1
    for n, group in by_n.items():
        vals = [r["pctRadio"] for r in group]
        spread = max(vals) - min(vals)
        if spread > spread_best:
            n_best, spread_best = n, spread
    return n_best, sorted(by_n[n_best], key=lambda r: r["pctRadio"])


def chart_series(rows):
    """avg pctRadio by horizonCoeff -> [values ordered by ascending nBalloons],
    matching the `data` object shape sweep-chart.html expects."""
    n_values = sorted({r["nBalloons"] for r in rows})
    coeffs = sorted({r["horizonCoeff"] for r in rows})
    series = {}
    for c in coeffs:
        by_n = defaultdict(list)
        for r in rows:
            if r["horizonCoeff"] == c:
                by_n[r["nBalloons"]].append(r["pctRadio"])
        series[f"{c:g}"] = [mean(by_n[n]) for n in n_values]
    return {"xLabels": [str(n) for n in n_values], "series": series}


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("csv_path")
    ap.add_argument("--chart-json", help="path to write chart series data as JSON (default: <csv_dir>/chart-data.json)")
    args = ap.parse_args()

    rows = load_rows(args.csv_path)
    if not rows:
        print(f"No rows in {args.csv_path}", file=sys.stderr)
        sys.exit(1)

    print(f"# Sweep summary tables — {args.csv_path} ({len(rows)} rows)\n")

    print("## Avg % delivered via radio, by balloon count\n")
    print(markdown_table(
        ["Balloon count", "Avg % radio"],
        [(n, f"{v:.1f}%") for n, v in avg_by(rows, "nBalloons").items()],
    ))

    print("\n## Avg % delivered via radio, by horizon coefficient\n")
    print(markdown_table(
        ["Horizon coeff", "Avg % radio"],
        [(c, f"{v:.1f}%") for c, v in avg_by(rows, "horizonCoeff").items()],
    ))

    print("\n## Avg % delivered via radio, by satellite-fallback timeout\n")
    print(markdown_table(
        ["Timeout (min)", "Avg % radio"],
        [(t, f"{v:.1f}%") for t, v in avg_by(rows, "fallbackTimeoutMin").items()],
    ))

    n_best, detail_rows = transition_zone(rows)
    print(f"\n## Transition-zone detail (nBalloons={n_best}, widest spread of pctRadio)\n")
    print(markdown_table(
        ["Horizon coeff", "Timeout (min)", "% radio"],
        [(r["horizonCoeff"], r["fallbackTimeoutMin"], f"{r['pctRadio']:.1f}%") for r in detail_rows],
    ))
    print(f"\nSpread: {detail_rows[0]['pctRadio']:.1f}% – {detail_rows[-1]['pctRadio']:.1f}%")

    chart_data = chart_series(rows)
    out_path = args.chart_json or (args.csv_path.rsplit("/", 1)[0] + "/chart-data.json" if "/" in args.csv_path else "chart-data.json")
    with open(out_path, "w") as f:
        json.dump(chart_data, f, indent=2)
    print(f"\nChart series data written to {out_path}", file=sys.stderr)


if __name__ == "__main__":
    main()
