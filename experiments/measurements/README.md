# Raw measurements — console output backing `aggregation-summary.md`

Two of the experiment binaries print a table to stdout and write no CSV, so
until now nothing they produced was committed and the figures quoting them in
[`../aggregation-summary.md`](../aggregation-summary.md) had no backing file in
the repo. These are those runs, captured verbatim.

The convention matches
[`../../docs/investigations/measurements/`](../../docs/investigations/measurements/):
one `.txt` per run, the exact command that produced it, and no editing of the
output.

| File | What it is | Backs |
|---|---|---|
| `protocol-compare.txt` | Every protocol over identical balloon fields. 8 seeds, n = 1200, 400 rounds, zero wind — the same 400 rounds every CSV experiment uses, so the numbers are directly comparable to them. | The `reply=tower` and spray-and-wait rows of the "how this compares to not routing at all" coda, plus the cross-protocol counters |
| `link-churn-zero.txt` | Link turnover with no wind — the control. n = 1200, 400 rounds. | The churn coda's control column |
| `link-churn-wind.txt` | The same measurement under a real ERA5 field, with the zero-wind control repeated inline for comparison. | The churn coda's 4.1× turnover result |

Reproduce with:

```bash
cd sim-server && cargo build --release
cd ..
./sim-server/target/release/protocol_compare 8 1200 400 \
  > experiments/measurements/protocol-compare.txt
./sim-server/target/release/link_churn none 1200 400 \
  > experiments/measurements/link-churn-zero.txt
./sim-server/target/release/link_churn 1978-06-09T03:00:00 1200 400 \
  > experiments/measurements/link-churn-wind.txt
```

`link_churn`'s second form needs that wind field in the on-disk cache — check
with `cd sim-server && ./target/release/wind_cache list`, and populate it with
`wind_cache fetch` if it is missing. `protocol_compare` takes `[seeds] [n]
[rounds]` positionally and runs single-threaded, so it is the slowest of the
three for its size.

Neither binary resumes: each writes its whole table in one pass, so an
interrupted run is restarted rather than continued.

**Cross-checked against the CSV experiments.** Restricted to the same 8 seeds,
`discovery_sweep` reproduces every overlapping `protocol_compare` figure here to
the decimal, standard deviations included. The two harnesses are equivalent; the
write-ups prefer the sweep's 20-seed figures purely for the wider sample, and
fall back to this capture for `reply=tower` and spray-and-wait, which the sweep
does not run.

**On reading these:** `protocol_compare` aligns only the four keys that mean
the same thing under every protocol — `originated`, `delivered`, `resolved`,
`completion_rate`. Everything else is printed per protocol and unaligned on
purpose, because lining `stall_no_belief` up against `duplicate_arrivals` would
invent a correspondence that does not exist. `link_churn`'s half-life is the
number to read; degree is there to show density did *not* move, which is what
makes the turnover result mean anything.
