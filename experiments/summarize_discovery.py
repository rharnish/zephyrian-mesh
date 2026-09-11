#!/usr/bin/env python3
"""Paired tables for the discovery sweep: do the AODV and OLSR mechanisms pay?

    python3 experiments/summarize_discovery.py experiments/discovery-sweep-results.csv

Every variant runs over an identical balloon field at a given seed, because the
protocol's RNG is independent of the world's. So each contrast is taken *within*
a seed and only then averaged. That matters more here than anywhere else in this
project: between-seed spread on completion is 6-8 points while the effects under
test are 1-3, so unpaired means cannot separate a real small effect from noise.

Two delivery ratios are reported side by side, because they disagree:

    completion_rate    delivered / resolved     (of bundles that finished)
    delivered/orig     delivered / originated   (of bundles that started)

A run that strands bundles in queues never resolves them, so they leave the
first denominator but not the second. Reactive discovery strands a great many,
which is exactly why it looks better on the first ratio than on the second.
Quoting only one would be a choice rather than a measurement.

`unresolved %` is the gap between them made explicit — the share of originated
bundles still being carried when the run ended. Read it first: it says how much
the two ratios are arguing about, and a variant with a low value is one where
they agree and either can be quoted safely.
"""
import csv
import math
import sys
from collections import defaultdict

# (label, baseline it is compared against). None means "no contrast, level only".
CONTRASTS = [
    ("proactive", None),
    ("proactive+digest", "proactive"),
    ("proactive+mesh4", "proactive"),
    ("proactive+digest+mesh4", "proactive"),
    ("proactive+mesh8", "proactive"),
    ("reactive", None),
    ("reactive+overhear", "reactive"),
    ("reactive+ring", "reactive"),
    ("reactive+overhear+ring", "reactive"),
    ("linkstate-lsa2", None),
    ("linkstate-lsa2-mpr", "linkstate-lsa2"),
    ("linkstate", None),
    ("linkstate-mpr", "linkstate"),
    ("linkstate-lsa16", None),
    ("linkstate-lsa16-mpr", "linkstate-lsa16"),
    ("linkstate-lsa64", None),
]

MECHANISM_KEYS = [
    "stall_no_belief",
    "satellite",
    "dropped_loop",
    "blocked",
    "belief_hops_mean",
    "delivered_hops_mean",
    "gossip_redundant_share",
    "mpr_share",
]


def mean(v):
    return sum(v) / len(v) if v else float("nan")


def sd(v):
    if len(v) < 2:
        return 0.0
    m = mean(v)
    return math.sqrt(sum((x - m) ** 2 for x in v) / (len(v) - 1))


def sem(v):
    return sd(v) / math.sqrt(len(v)) if len(v) > 1 else 0.0


def load(path):
    """-> rows[label][seed][key] = float, skipping blanks for keys a variant
    does not keep (a link-state counter under reactive, say)."""
    rows = defaultdict(dict)
    with open(path) as f:
        for r in csv.DictReader(f):
            vals = {}
            for k, v in r.items():
                if k in ("protocol", "seed") or v is None or v == "":
                    continue
                try:
                    vals[k] = float(v)
                except ValueError:
                    pass
            rows[r["protocol"]][int(r["seed"])] = vals
    return rows


def paired(rows, label, base, key):
    """Per-seed differences, over seeds where both variants have the key."""
    a, b = rows.get(label, {}), rows.get(base, {})
    return [
        a[s][key] - b[s][key]
        for s in sorted(set(a) & set(b))
        if key in a[s] and key in b[s]
    ]


def main():
    path = sys.argv[1] if len(sys.argv) > 1 else "experiments/discovery-sweep-results.csv"
    rows = load(path)
    seeds = max(len(v) for v in rows.values())
    print(f"# Discovery sweep — {seeds} seeds, paired within seed\n")

    print("## Levels\n")
    print("| variant | completion % | delivered/orig % | unresolved % | stall rate |")
    print("|---|---|---|---|---|")
    for label, _ in CONTRASTS:
        if label not in rows:
            continue
        c = [v["completion_rate"] * 100 for v in rows[label].values()]
        d = [v["delivered_per_originated"] * 100 for v in rows[label].values()]
        u = [v.get("unresolved_share", 0) * 100 for v in rows[label].values()]
        s = [v.get("stall_rate", 0) for v in rows[label].values()]
        print(
            f"| {label} | {mean(c):.1f} ± {sd(c):.1f} | {mean(d):.1f} ± {sd(d):.1f} "
            f"| {mean(u):.1f} | {mean(s):.3f} |"
        )

    print("\n## Latency, in comms rounds (5 rounds = one wake slot)\n")
    print("| variant | first hop | delivery | p95 | ack round trip |")
    print("|---|---|---|---|---|")
    for label, _ in CONTRASTS:
        if label not in rows:
            continue

        def m(key):
            return mean([v.get(key, 0) for v in rows[label].values()])

        print(
            f"| {label} | {m('first_hop_latency_mean'):.1f} "
            f"| {m('delivery_latency_mean'):.1f} | {m('delivery_latency_p95'):.0f} "
            f"| {m('ack_latency_mean'):.1f} |"
        )

    print("\n## Paired contrasts (mean difference ± 1 s.e.)\n")
    print(
        "| contrast | Δ completion | Δ delivered/orig | Δ delivery latency | Δ ack latency |"
    )
    print("|---|---|---|---|---|")
    for label, base in CONTRASTS:
        if base is None or label not in rows:
            continue
        c = [x * 100 for x in paired(rows, label, base, "completion_rate")]
        d = [x * 100 for x in paired(rows, label, base, "delivered_per_originated")]
        lat = paired(rows, label, base, "delivery_latency_mean")
        ack = paired(rows, label, base, "ack_latency_mean")
        print(
            f"| {label} vs {base} | {mean(c):+.2f} ± {sem(c):.2f} "
            f"| {mean(d):+.2f} ± {sem(d):.2f} "
            f"| {mean(lat):+.1f} ± {sem(lat):.1f} | {mean(ack):+.1f} ± {sem(ack):.1f} |"
        )

    print("\n## Mechanism counters (paired means, so *why* it moved)\n")
    for label, base in CONTRASTS:
        if base is None or label not in rows:
            continue
        parts = []
        for k in MECHANISM_KEYS:
            diffs = paired(rows, label, base, k)
            if not diffs:
                continue
            b = [rows[base][s][k] for s in sorted(rows[base]) if k in rows[base][s]]
            parts.append(f"    {k:24} {mean(b):10.2f} -> {mean(b) + mean(diffs):10.2f}")
        if parts:
            print(f"{label} vs {base}:")
            print("\n".join(parts))
            print()


if __name__ == "__main__":
    main()
