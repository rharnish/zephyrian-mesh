#!/usr/bin/env python3
"""
Renders a static PNG line chart (avg % delivered via radio vs. balloon
count, one line per horizon coefficient) from a connectivity-sweep results
CSV — same data summarize_sweep.py summarizes into markdown tables and
chart-data.json, but as an image suitable for embedding directly in a
sweep-summary-*.md with a markdown ![]() tag, since those docs live in
version control and an interactive HTML chart doesn't render there.

Balloon counts not swept for a given coefficient (e.g. a denser sub-sweep
added only for some coefficients) are left as gaps in that coefficient's
line rather than errors, so the sweep grid doesn't have to be a full
rectangle.

Usage:
    python3 experiments/plot_sweep.py experiments/connectivity-sweep-results-rust.csv \
        --out experiments/results-rust/sweep-chart.png \
        --title "Radio delivery % by balloon count, split by horizon coefficient"

    python3 experiments/plot_sweep.py experiments/connectivity-sweep-results-rust.csv \
        --out experiments/results-rust/sweep-chart-transition-zoom.png \
        --coeffs 3.4,4.0 --n-values 400,500,600,700,800,900,1000,1600 \
        --title "Radio delivery %, zoomed into the transition zone"
"""

import argparse
import csv
from statistics import mean

import matplotlib

matplotlib.use("Agg")
import matplotlib.pyplot as plt

# Same palette as the interactive sweep-chart.html templates, in coefficient order.
SERIES_COLORS = ["#2a78d6", "#008300", "#e87ba4", "#eda100"]

# Opacity levels for split-by series (first value fully opaque, later values
# progressively lighter) — cycled if a split column has >4 values.
ALPHAS = [1.0, 0.4, 0.7, 0.25]


def load_rows(csv_path):
    rows = []
    with open(csv_path, newline="") as f:
        for r in csv.DictReader(f):
            r["horizonCoeff"] = float(r["horizonCoeff"])
            r["nBalloons"] = int(r["nBalloons"])
            r["pctRadio"] = float(r["pctRadio"])
            rows.append(r)
    return rows


def build_series(rows, coeffs, n_values, split_by=None, split_values=None):
    if split_by is None:
        series = {}
        for c in coeffs:
            vals = []
            for n in n_values:
                matches = [r["pctRadio"] for r in rows if r["horizonCoeff"] == c and r["nBalloons"] == n]
                vals.append(mean(matches) if matches else None)
            series[(c, None)] = vals
        return series

    if split_values is None:
        split_values = sorted({r[split_by] for r in rows}, key=lambda v: (len(v), v))

    series = {}
    for c in coeffs:
        for sv in split_values:
            vals = []
            for n in n_values:
                matches = [
                    r["pctRadio"] for r in rows
                    if r["horizonCoeff"] == c and r["nBalloons"] == n and r[split_by] == sv
                ]
                vals.append(mean(matches) if matches else None)
            series[(c, sv)] = vals
    return series


def plot(n_values, series, out_path, title, subtitle=None, linear_x=False, split_by=None, split_label=None):
    fig, ax = plt.subplots(figsize=(8.5, 4.6), dpi=150)
    xs = list(n_values) if linear_x else list(range(len(n_values)))
    coeff_order = []
    for coeff, _sv in series:
        if coeff not in coeff_order:
            coeff_order.append(coeff)
    split_order = []
    for _coeff, sv in series:
        if sv is not None and sv not in split_order:
            split_order.append(sv)

    for (coeff, sv), vals in series.items():
        color = SERIES_COLORS[coeff_order.index(coeff) % len(SERIES_COLORS)]
        alpha = ALPHAS[split_order.index(sv) % len(ALPHAS)] if sv is not None else 1.0
        label = f"horizon coeff {coeff:.1f}" if sv is None else f"horizon coeff {coeff:.1f}, {split_label or split_by} {sv}"
        # break the line across gaps instead of interpolating over missing points
        seg_xs, seg_vals = [], []
        for x, v in zip(xs, vals):
            if v is None:
                if seg_xs:
                    ax.plot(seg_xs, seg_vals, marker="o", color=color, alpha=alpha, linewidth=2, markersize=5)
                seg_xs, seg_vals = [], []
                continue
            seg_xs.append(x)
            seg_vals.append(v)
        if seg_xs:
            ax.plot(seg_xs, seg_vals, marker="o", color=color, alpha=alpha, linewidth=2, markersize=5, label=label)

    if linear_x:
        ax.set_xlim(min(xs) - 0.03 * max(xs), max(xs) * 1.03)
    else:
        ax.set_xticks(xs)
        ax.set_xticklabels([str(n) for n in n_values])
    ax.set_xlabel("Balloon count")
    ax.set_ylabel("% delivered via radio")
    ax.set_ylim(-3, 103)
    ax.set_yticks(range(0, 101, 25))
    ax.grid(axis="y", alpha=0.25)
    ax.spines["top"].set_visible(False)
    ax.spines["right"].set_visible(False)
    ax.legend(frameon=False, loc="upper left")
    ax.set_title(title, fontsize=12, fontweight="bold", loc="left", pad=28 if subtitle else 10)
    if subtitle:
        ax.text(0, 1.05, subtitle, transform=ax.transAxes, fontsize=9, color="#666666")

    fig.tight_layout()
    fig.savefig(out_path)
    print(f"Wrote {out_path}")


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("csv_path")
    ap.add_argument("--out", required=True, help="output PNG path")
    ap.add_argument("--coeffs", help="comma-separated horizon coefficients to plot (default: all present in CSV)")
    ap.add_argument("--n-values", help="comma-separated balloon counts to plot (default: all present in CSV)")
    ap.add_argument("--title", default="Radio delivery % by balloon count, split by horizon coefficient")
    ap.add_argument("--subtitle", default=None)
    ap.add_argument("--linear-x", action="store_true", help="use a true numeric x-axis instead of evenly-spaced categories")
    ap.add_argument("--split-by", help="CSV column to split each coefficient into multiple opacity-differentiated series (e.g. fallbackTimeoutMin)")
    ap.add_argument("--split-values", help="comma-separated values of --split-by to include (default: all present)")
    ap.add_argument("--split-label", help="display name for --split-by in the legend (default: the raw column name, e.g. fallbackTimeoutMin)")
    args = ap.parse_args()

    rows = load_rows(args.csv_path)
    coeffs = [float(c) for c in args.coeffs.split(",")] if args.coeffs else sorted({r["horizonCoeff"] for r in rows})
    n_values = [int(n) for n in args.n_values.split(",")] if args.n_values else sorted({r["nBalloons"] for r in rows})
    split_values = args.split_values.split(",") if args.split_values else None

    series = build_series(rows, coeffs, n_values, split_by=args.split_by, split_values=split_values)
    plot(n_values, series, args.out, args.title, args.subtitle, linear_x=args.linear_x, split_by=args.split_by, split_label=args.split_label)


if __name__ == "__main__":
    main()
