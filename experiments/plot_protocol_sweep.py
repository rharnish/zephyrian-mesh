#!/usr/bin/env python3
"""
Renders a static PNG chart from a protocol_sweep.rs results CSV: omniscient
ground truth (groundedPct, from union-find) vs. what balloons *believe*
(believedGroundedPct) vs. what the real decentralized protocol actually
delivers — completion rate, raw delivered/originated, and delivered-but-
unacknowledged/originated — against mean node degree, one panel per balloon
count, with horizon coefficient moving each panel along its x axis.

This used to be a single panel, on the strength of docs/design/MESH_COMMS_DESIGN.md
§1.1's finding that sweeps over balloon count and horizon coefficient collapse
onto one curve when read by degree. For the real protocol they don't. With 10
seeds per cell the spread is a few points, yet cells of near-equal degree still
differ by 15–20 points of completion: at degree ~3.5, 400 balloons at horizon
5.0 complete 40% while 2000 balloons at 2.5 complete 22%. Longer links reach
a tower in fewer hops, which degree alone doesn't capture. A single line through
all cells in degree order drew that as a sawtooth, so the balloon counts are
kept apart.

Delivered/originated and completion rate (delivered/resolved) are both shown
deliberately, not just one: the gap between them *is* the censoring bias from
bundles still legitimately in flight at the cutoff (see BundleStats::
completion_rate's own doc comment) — the same distinction bundle_delivery.rs's
"deliv/orig" vs "completion" columns make.

"Resolved" is not "acked". A bundle is delivered the moment a tower takes it,
before any ack exists, and resolved means it left circulation by any route —
delivered, satellite, or dropped. The ack's fate is split out separately as
acked and ack-lost, each a share of originated; what's left of delivered is
acks still in flight at the cutoff.

Points are the sweep's own (horizonCoeff, nBalloons) cells, connected in
degree order within a balloon count — not a fitted curve. The CSV holds one row per seed; each point
is the mean over a cell's seeds and the band is ±1 standard deviation, so the
run-to-run spread near the percolation transition is drawn as spread rather
than as wiggles in a single line. Degree, grounded % and believed % are
already run-averaged by protocol_sweep.rs (see its header).

Usage:
    python3 experiments/plot_protocol_sweep.py experiments/protocol-sweep-results.csv \
        --out experiments/protocol-results/truth-vs-belief-vs-delivery.png
"""

import argparse
import csv
import statistics
from collections import defaultdict

import matplotlib

matplotlib.use("Agg")
import matplotlib.pyplot as plt

# Borrowed from the app's own belief-vs-truth overlay (src/main.js BELIEF_COLORS)
# so this chart's color language matches what the running app already uses for
# the same distinction, rather than inventing a new one.
#
# Shared with plot_protocol_sweep_html.py's light palette, so the PNG and its
# HTML twin name every series the same way. Validated in this order (dataviz
# validate_palette.js, adjacent pairs, light surface): no normal-vision pair
# under 15. Two documented misses, both inherited from the app: truth green's
# OKLCH lightness 0.774 sits 0.004 over the band, and truth<->belief is CVD
# ΔE 6.2 — legal only with secondary encoding, which the legend order and the
# fact that belief never exceeds truth by more than noise provide.
COLOR_TRUTH = "#5fd08a"  # "ok" green — omniscient ground truth (union-find)
COLOR_BELIEF = "#e0a355"  # "unaware" amber — what balloons currently believe
COLOR_ACHIEVED = "#2a78d6"  # blue, dataviz slot 1 — completion rate
COLOR_DELIVERED = "#e87ba4"  # magenta, slot 5 — reached a tower, % of originated
COLOR_ACKED = "#008300"  # green, slot 6 — reached a tower and the ack came home
COLOR_UNACKED = "#4a3aa7"  # violet, slot 7 — reached a tower, ack lost. Was red
#                            #e05561, which sat ΔE 10.4 from the magenta beside it.


# Everything plotted, per seed row. Percentages throughout, so the y axis is one scale.
METRICS = {
    "degree": lambda r: r["meanDegree"],
    "truth": lambda r: r["groundedPct"],
    "belief": lambda r: r["believedGroundedPct"],
    "completion": lambda r: 100.0 * r["completionRate"],
    "delivered": lambda r: 100.0 * r["delivered"] / r["originated"] if r["originated"] else 0.0,
    "acked": lambda r: 100.0 * r["ackedCount"] / r["originated"] if r["originated"] else 0.0,
    "unacked": lambda r: 100.0 * r["ackLostCount"] / r["originated"] if r["originated"] else 0.0,
}


def load_cells(csv_path):
    """One entry per (horizonCoeff, nBalloons) cell: mean and sd of each metric over its seeds."""
    by_cell = defaultdict(list)  # (horizonCoeff, nBalloons) -> seed rows
    with open(csv_path, newline="") as f:
        for r in csv.DictReader(f):
            row = {k: float(v) for k, v in r.items()}
            by_cell[(row["horizonCoeff"], row["nBalloons"])].append(row)

    cells = []
    for rows in by_cell.values():
        cell = {"seeds": len(rows), "horizon": rows[0]["horizonCoeff"], "n": int(rows[0]["nBalloons"]),
                "originated": statistics.fmean(r["originated"] for r in rows)}
        for name, fn in METRICS.items():
            values = [fn(r) for r in rows]
            cell[name] = statistics.fmean(values)
            cell[name + "_sd"] = statistics.stdev(values) if len(values) > 1 else 0.0
        cells.append(cell)
    cells.sort(key=lambda c: c["degree"])
    return cells


# (metric key, legend label, colour) — also the HTML twin's series, in this order.
SERIES = [
    ("truth", "Ground truth: actually reachable (union-find)", COLOR_TRUTH),
    ("belief", "Belief: balloons that think they have a route", COLOR_BELIEF),
    # "Finished" = resolved: reached a tower, went by satellite, or was
    # dropped. Reaching a tower counts at hand-off, ack or no ack.
    ("completion", "Completion rate: reached a tower, % of finished bundles", COLOR_ACHIEVED),
    ("delivered", "Reached a tower, % of originated", COLOR_DELIVERED),
    ("acked", "Reached a tower and acked, % of originated", COLOR_ACKED),
    ("unacked", "Reached a tower, ack lost, % of originated", COLOR_UNACKED),
]

PERCOLATION_DEGREE = 4.5


def plot(cells, out_path, title, subtitle=None):
    ns = sorted({c["n"] for c in cells})
    fig, axes = plt.subplots(1, len(ns), figsize=(4.2 * len(ns), 5.2), dpi=150, sharey=True, squeeze=False)
    axes = axes[0]
    for ax, n in zip(axes, ns):
        panel = [c for c in cells if c["n"] == n]
        xs = [c["degree"] for c in panel]
        for key, label, color in SERIES:
            ys = [c[key] for c in panel]
            sds = [c[key + "_sd"] for c in panel]
            ax.fill_between(xs, [y - sd for y, sd in zip(ys, sds)], [y + sd for y, sd in zip(ys, sds)],
                            color=color, alpha=0.15, linewidth=0)
            ax.plot(xs, ys, marker="o", color=color, linewidth=2, markersize=5, label=label)

        # Which horizon coefficient each point is, on a top axis.
        top = ax.secondary_xaxis("top")
        top.set_xticks(xs, labels=[f"{c['horizon']:g}" for c in panel])
        top.tick_params(labelsize=7, colors="#888888", length=2)
        top.set_xlabel("horizon coefficient", fontsize=8, color="#888888")

        pad = 0.06 * (max(xs) - min(xs))
        ax.set_xlim(min(xs) - pad, max(xs) + pad)
        # Percolation threshold tick, same convention as the Controls panel's
        # mesh-health readout in the running app — only where it's in range.
        if min(xs) - pad < PERCOLATION_DEGREE < max(xs) + pad:
            ax.axvline(PERCOLATION_DEGREE, color="#999999", linewidth=1, linestyle="--", zorder=0)
            ax.text(PERCOLATION_DEGREE, 2, " percolation threshold", fontsize=7, color="#777777",
                    rotation=90, ha="right", va="bottom")

        ax.set_title(f"{n} balloons", fontsize=10, fontweight="bold", loc="left", pad=6)
        ax.set_xlabel("Mean node degree", fontsize=9)
        ax.set_ylim(-3, 103)
        ax.set_yticks(range(0, 101, 25))
        ax.grid(axis="y", alpha=0.25)
        ax.spines["top"].set_visible(False)
        ax.spines["right"].set_visible(False)
    axes[0].set_ylabel("%")

    handles, labels = axes[0].get_legend_handles_labels()
    fig.legend(handles, labels, frameon=False, loc="lower center", ncol=3, fontsize=9)
    fig.suptitle(title, fontsize=12, fontweight="bold", x=0.01, ha="left", y=0.985)
    if subtitle:
        fig.text(0.01, 0.935, subtitle, fontsize=9, color="#666666", ha="left")

    fig.tight_layout(rect=(0, 0.1, 1, 0.93))
    fig.savefig(out_path)
    print(f"Wrote {out_path}")


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("csv_path")
    ap.add_argument("--out", required=True, help="output PNG path")
    ap.add_argument("--title", default="Truth vs. belief vs. real delivery, by mesh density")
    ap.add_argument("--subtitle", help="default names the protocol and the seed count read from the CSV")
    args = ap.parse_args()

    cells = load_cells(args.csv_path)
    seeds = min(c["seeds"] for c in cells)
    subtitle = args.subtitle or (
        f"dv-dtn (shipped defaults), real wind, 24 sim-h runs — mean of {seeds} seeds per point, band ±1 sd — protocol_sweep.rs"
    )
    plot(cells, args.out, args.title, subtitle)


if __name__ == "__main__":
    main()
