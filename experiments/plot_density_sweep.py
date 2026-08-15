#!/usr/bin/env python3
"""Renders a chart from density_sweep.rs's per-seed CSV: does the protocol
ranking `aggregation-summary.md` found at a single density (n=1200, 4 seeds —
`dv-dtn:ack=digest,mesh=4` far ahead at 95.0 ± 1.4% completion vs. 72.3 ± 7.7%
for shipped dv-dtn) hold as balloon count changes, or was it an artifact of
that one density?

Every protocol sees an identical balloon field at a given seed (the protocol
RNG is independent of the world's), so within each (protocol, n) cell the
seeds are paired across protocols even though this script only needs the
per-cell mean and standard error, not a cross-protocol contrast.

Two panels, both vs. balloon count (log x-axis, matching the `h*_n*.json`
density grid this repo already sweeps):

  - completion_rate           = delivered / resolved   (of bundles that finished)
  - delivered_per_originated  = delivered / originated (of bundles that started)

Both are shown, not just one, because they disagree: a protocol that strands
bundles in queues at the cutoff never resolves them, so they leave the first
denominator and not the second (see discovery_sweep.rs's own header comment,
and summarize_discovery.py). Reporting only completion_rate would flatter
whichever protocol strands the most bundles.

11 lines is above the categorical palette's 8-slot cap (dataviz skill), so
color here is composite: family (hue) x role (line weight/style). Three
"headline" contenders get solid, full-weight lines in distinct hues; every
other variant is a thin, lower-alpha dashed line in its family's hue.

A ground-truth reachability line (--truth) can be overlaid on both panels:
the % of balloons whose physical connectivity component contains a tower
(union-find, from `ground_truth_sweep.rs`), computed before any protocol is
consulted — the percolation ceiling no protocol can exceed regardless of how
good its routing is. It is the same color the app's own belief-vs-truth
overlay uses for this exact quantity (src/main.js BELIEF_COLORS), so this
chart's color language matches what's already on screen elsewhere.

Usage:
    python3 experiments/plot_density_sweep.py experiments/density-sweep-results.csv \
        --out experiments/protocol-results/density-sweep.png \
        --truth experiments/ground-truth-sweep-results.csv
"""

import argparse
import csv
import math
from collections import defaultdict

import matplotlib

matplotlib.use("Agg")
import matplotlib.pyplot as plt

# dataviz skill categorical slots 1/2/3/4/8 (blue/orange/aqua/yellow/red) —
# validated (`node scripts/validate_palette.js`): CVD-safe adjacent pairs,
# WARN on aqua/yellow contrast vs. the light surface mitigated here by the
# always-on legend + line markers rather than color alone.
BLUE = "#2a78d6"
ORANGE = "#eb6834"
AQUA = "#1baf7a"
YELLOW = "#eda100"
RED = "#e34948"
# Ground truth: "ok" green, matching src/main.js's BELIEF_COLORS and
# plot_protocol_sweep.py's COLOR_TRUTH — not one of the 5 categorical slots
# above, so it reads as a reference ceiling rather than a 12th contender.
TRUTH = "#5fd08a"

# label -> (color, is_headline). Headline = solid, full weight, in the legend
# first. Everything else in a family shares its hue, thin and dashed.
STYLE = {
    "dv-dtn": (BLUE, True),
    "digest-mesh4": (ORANGE, True),
    "spray-l16": (RED, True),
    "spray-l4": (RED, False),
    "reactive": (YELLOW, True),
    "reactive-tower": (YELLOW, False),
    "reactive-overhear": (YELLOW, False),
    "reactive-ring": (YELLOW, False),
    "reactive-overhear-ring": (YELLOW, False),
    "linkstate": (AQUA, True),
    "linkstate-mpr": (AQUA, False),
}

# Legend order: headline contenders first, then each family grouped together.
LABEL_ORDER = [
    "dv-dtn",
    "digest-mesh4",
    "spray-l16",
    "spray-l4",
    "reactive",
    "reactive-tower",
    "reactive-overhear",
    "reactive-ring",
    "reactive-overhear-ring",
    "linkstate",
    "linkstate-mpr",
]


def mean(v):
    return sum(v) / len(v) if v else float("nan")


def sem(v):
    if len(v) < 2:
        return 0.0
    m = mean(v)
    sd = math.sqrt(sum((x - m) ** 2 for x in v) / (len(v) - 1))
    return sd / math.sqrt(len(v))


def load(csv_path):
    """-> {label: {n: {"completion_rate": [...], "delivered_per_originated": [...]}}}"""
    data = defaultdict(lambda: defaultdict(lambda: defaultdict(list)))
    with open(csv_path, newline="") as f:
        for row in csv.DictReader(f):
            label = row["protocol"]
            n = int(row["n"])
            for key in ("completion_rate", "delivered_per_originated"):
                data[label][n][key].append(float(row[key]))
    return data


def load_truth(csv_path):
    """-> {n: [groundedPct, ...]} — protocol-independent, so no label dimension."""
    data = defaultdict(list)
    with open(csv_path, newline="") as f:
        for row in csv.DictReader(f):
            data[int(row["n"])].append(float(row["groundedPct"]))
    return data


def plot_panel(ax, data, key, ylabel, truth=None):
    if truth:
        ns = sorted(truth.keys())
        means = [mean(truth[n]) for n in ns]
        errs = [sem(truth[n]) for n in ns]
        lo = [m - e for m, e in zip(means, errs)]
        hi = [m + e for m, e in zip(means, errs)]
        ax.plot(ns, means, color=TRUTH, linewidth=2.2, linestyle=":", marker="^",
                 markersize=5, label="ground truth (reachable)", zorder=4)
        ax.fill_between(ns, lo, hi, color=TRUTH, alpha=0.15, zorder=2, linewidth=0)

    for label in LABEL_ORDER:
        if label not in data:
            continue
        color, headline = STYLE[label]
        ns = sorted(data[label].keys())
        means = [100.0 * mean(data[label][n][key]) for n in ns]
        errs = [100.0 * sem(data[label][n][key]) for n in ns]
        lo = [m - e for m, e in zip(means, errs)]
        hi = [m + e for m, e in zip(means, errs)]

        if headline:
            ax.plot(ns, means, color=color, linewidth=2.2, marker="o", markersize=5,
                     label=label, zorder=3)
            ax.fill_between(ns, lo, hi, color=color, alpha=0.15, zorder=2, linewidth=0)
        else:
            ax.plot(ns, means, color=color, linewidth=1.1, linestyle="--", alpha=0.55,
                     label=label, zorder=1)

    ax.set_xscale("log")
    ax.set_ylabel(ylabel)
    ax.set_ylim(-3, 103)
    ax.set_yticks(range(0, 101, 25))
    ax.grid(axis="y", alpha=0.25)
    ax.spines["top"].set_visible(False)
    ax.spines["right"].set_visible(False)


def plot(data, out_path, title, subtitle, truth=None):
    fig, (ax1, ax2) = plt.subplots(2, 1, figsize=(10, 8.5), dpi=150, sharex=True)

    plot_panel(ax1, data, "completion_rate", "Completion rate (%)\ndelivered / resolved", truth=truth)
    plot_panel(ax2, data, "delivered_per_originated",
               "Delivered (%)\ndelivered / originated", truth=truth)
    ax2.set_xlabel("Balloon count (n)")

    handles, labels = ax1.get_legend_handles_labels()
    fig.legend(handles, labels, loc="center left", bbox_to_anchor=(1.0, 0.5),
               frameon=False, fontsize=8.5)

    ax1.set_title(title, fontsize=12, fontweight="bold", loc="left", pad=28 if subtitle else 14)
    if subtitle:
        ax1.text(0, 1.08, subtitle, transform=ax1.transAxes, fontsize=9, color="#666666")

    fig.tight_layout()
    fig.savefig(out_path, bbox_inches="tight")
    print(f"Wrote {out_path}")


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("csv_path")
    ap.add_argument("--out", required=True, help="output PNG path")
    ap.add_argument("--truth", help="ground_truth_sweep.rs CSV (n,seed,meanDegree,groundedPct) to overlay")
    ap.add_argument("--title", default="Protocol density sweep: does the ranking hold as n changes?")
    ap.add_argument("--subtitle", default="dv-dtn variants, reactive/link-state discovery, spray-and-wait — zero wind, density_sweep.rs")
    args = ap.parse_args()

    data = load(args.csv_path)
    truth = load_truth(args.truth) if args.truth else None
    plot(data, args.out, args.title, args.subtitle, truth=truth)


if __name__ == "__main__":
    main()
