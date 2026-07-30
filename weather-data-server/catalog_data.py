#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
Inventory the weather data files in `data/` and write a JSON catalog.

Run it:
    ./.venv/bin/python catalog_data.py            # write data/catalog.json
    ./.venv/bin/python catalog_data.py --print    # look, don't write

Copernicus hands you opaque hash filenames like
`b2436709633b357f9ec6d77da9772ea0.nc`, which tell you nothing about what's
inside. This answers "what is actually in these things" once, cheaply, and in
a form both a human and a program can read.

This is an *inspection tool*, not part of the server's startup path -- the
server reads `wind_source.json`, which you edit by hand. Use the output here
to decide what to put in it.

METADATA ONLY. This opens every dataset but never touches the wind arrays --
it reads dimensions, variable names, coordinates and global attributes. That
matters because the files run to 8GB; cataloging one must stay a header read,
not a 334MB-per-hour disk crawl.
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import os
import sys
import traceback

import numpy as np
import xarray as xr

from wind_data import describe_wind_source

CATALOG_VERSION = 1

DATA_EXTENSIONS = {".nc": "netcdf4", ".grib": "cfgrib", ".grib2": "cfgrib"}

# Bytes of JSON text per float in the /api/wind-levels payload. Calibrated
# against the measurement in docs/investigations/WIND_TRANSFER_PERF.md (~356MB
# for one stride-2 step, i.e. ~19.2M floats) so the estimate printed here and
# the number in that document agree rather than telling two stories.
JSON_BYTES_PER_FLOAT = 19.2

# Above this many time steps, store only the first and last timestamp so the
# catalog stays something a human can skim.
MAX_TIME_VALUES = 200


# ---------------------------------------------------------------------------
# Small helpers
# ---------------------------------------------------------------------------
def _iso(value) -> str:
    """numpy datetime64 (or anything datetime-ish) -> ISO 8601 string."""
    return np.datetime_as_string(np.datetime64(value), unit="s").item()


def _mtime_iso(path: str) -> str:
    return (
        dt.datetime.fromtimestamp(os.path.getmtime(path), dt.timezone.utc)
        .replace(microsecond=0)
        .isoformat()
        .replace("+00:00", "Z")
    )


def _jsonable(value):
    """numpy scalars/arrays -> plain Python, so json.dump doesn't choke on the
    int64/float32 values that come straight off xarray attrs."""
    if isinstance(value, np.generic):
        return value.item()
    if isinstance(value, np.ndarray):
        return [_jsonable(v) for v in value.tolist()]
    if isinstance(value, dict):
        return {str(k): _jsonable(v) for k, v in value.items()}
    if isinstance(value, (list, tuple)):
        return [_jsonable(v) for v in value]
    return value


# ---------------------------------------------------------------------------
# Time span analysis
# ---------------------------------------------------------------------------
def _describe_time(ds, time_dim):
    """Group the time coordinate into runs of constant step.

    A file holding 24 consecutive hourly steps and a file holding 24 steps
    scattered across a month look identical if you only report a count, and
    they are not interchangeable: interpolating wind between two steps only
    means something when the steps are actually adjacent in time. So report
    the spans, with the step size, and flag whether the whole file is one
    gap-free hourly run.
    """
    values = np.sort(np.asarray(ds[time_dim].values).ravel())
    count = len(values)

    info = {"count": count, "truncated": False, "values": [], "spans": [], "regularHourly": False}

    if count == 0:
        return info

    if count <= MAX_TIME_VALUES:
        info["values"] = [_iso(v) for v in values]
    else:
        info["truncated"] = True
        info["values"] = [_iso(values[0]), _iso(values[-1])]

    if count == 1:
        info["spans"] = [
            {
                "startIso": _iso(values[0]),
                "endIso": _iso(values[0]),
                "count": 1,
                "stepSeconds": None,
                "contiguous": False,
            }
        ]
        return info

    steps = (np.diff(values) / np.timedelta64(1, "s")).astype(np.int64)

    spans = []
    run_start = 0
    for i in range(len(steps)):
        # A run ends when the next gap differs from the current one.
        if i + 1 < len(steps) and steps[i + 1] == steps[i]:
            continue
        spans.append(
            {
                "startIso": _iso(values[run_start]),
                "endIso": _iso(values[i + 1]),
                "count": int(i + 1 - run_start + 1),
                "stepSeconds": int(steps[i]),
                "contiguous": True,
            }
        )
        run_start = i + 1

    info["spans"] = spans
    info["regularHourly"] = len(spans) == 1 and spans[0]["stepSeconds"] == 3600
    return info


def _describe_grid(ds, lat_dim, lon_dim):
    lats = np.asarray(ds[lat_dim].values, dtype=float).ravel()
    lons = np.asarray(ds[lon_dim].values, dtype=float).ravel()

    def _step(arr):
        if len(arr) < 2:
            return None
        return abs(float(arr[1] - arr[0]))

    d_lat, d_lon = _step(lats), _step(lons)

    def _uniform(arr, step):
        # The frontend reconstructs grid coordinates arithmetically as
        # lon = lo1 + col*dx / lat = la1 - row*dy (src/windVectors.js), and the
        # Rust sampler does the inverse. Both silently misplace data on a
        # non-uniform grid rather than erroring, so record the assumption.
        if len(arr) < 3 or step in (None, 0.0):
            return True
        return bool(np.allclose(np.abs(np.diff(arr)), step, rtol=1e-6, atol=1e-9))

    return {
        "ny": len(lats),
        "nx": len(lons),
        "latFirst": float(lats[0]),
        "latLast": float(lats[-1]),
        "lonFirst": float(lons[0]),
        "lonLast": float(lons[-1]),
        "dLat": d_lat,
        "dLon": d_lon,
        "latDescending": bool(len(lats) > 1 and lats[0] > lats[-1]),
        "uniform": _uniform(lats, d_lat) and _uniform(lons, d_lon),
    }


def _describe_payload(n_levels, grid, stride):
    strided_ny = -(-grid["ny"] // stride)  # ceil division, matches slice(None, None, stride)
    strided_nx = -(-grid["nx"] // stride)
    floats = n_levels * strided_ny * strided_nx * 2  # u and v
    return {
        "spatialStride": stride,
        "stridedNy": strided_ny,
        "stridedNx": strided_nx,
        "floatsPerTimeStep": int(floats),
        "estimatedJsonMbPerTimeStep": round(floats * JSON_BYTES_PER_FLOAT / (1024 * 1024), 1),
        "estimatedF32MbPerTimeStep": round(floats * 4 / (1024 * 1024), 1),
    }


# ---------------------------------------------------------------------------
# Receipts
# ---------------------------------------------------------------------------
def _load_receipts(data_dir):
    """Copernicus download receipts, keyed by the hash of the file they
    describe.

    A receipt's "filename" field looks like `s3://cci2-prod-cache-1/<date>/
    <hash>.zip`, and that hash is the name Copernicus gave the download. So
    the hash is the join key between a receipt and the data file on disk --
    which is the only way to recover *what you actually asked for* (dataset,
    date, variables, licence) from an opaque filename.
    """
    receipts = {}
    unmatched = []
    for entry in sorted(os.listdir(data_dir)):
        if not entry.startswith("receipt-") or not entry.endswith((".txt", ".json")):
            continue
        path = os.path.join(data_dir, entry)
        try:
            with open(path) as fh:
                body = json.load(fh)
        except Exception as e:
            unmatched.append({"file": entry, "error": f"could not parse as JSON: {e}"})
            continue

        s3_name = os.path.basename(str(body.get("filename", "")))
        hash_key = os.path.splitext(s3_name)[0]
        record = {
            "receiptFile": entry,
            "collectionId": body.get("collection-id"),
            "request": _jsonable(body.get("request")),
            "licence": _jsonable(body.get("licence")),
            "hash": hash_key or None,
        }
        if hash_key:
            receipts[hash_key] = record
        else:
            unmatched.append({"file": entry, "error": "receipt has no usable 'filename' field"})
    return receipts, unmatched


def _provenance(ds_attrs, rel_path, receipts):
    """Receipt if we can join one by hash, otherwise fall back to whatever the
    file's own global attributes admit about its origin."""
    matched = None
    for hash_key, record in receipts.items():
        if hash_key and hash_key in rel_path:
            matched = record
            break

    return {
        "receiptFile": matched["receiptFile"] if matched else None,
        "collectionId": matched["collectionId"] if matched else None,
        "request": matched["request"] if matched else None,
        "licence": matched["licence"] if matched else None,
        "gribCentre": _jsonable(ds_attrs.get("GRIB_centre")),
        "institution": _jsonable(ds_attrs.get("institution")),
        "history": _jsonable(ds_attrs.get("history")),
    }, (matched or {}).get("hash")


# ---------------------------------------------------------------------------
# Per-file inspection
# ---------------------------------------------------------------------------
def _find_data_files(data_dir):
    """Recursive, because Copernicus .zip downloads extract into a
    subdirectory (`<hash>/data_stream-oper_*.nc`) and those are real data
    files worth reporting on -- even if, as with the single-level products,
    the answer is "not usable as a wind source"."""
    found = []
    for root, _dirs, files in os.walk(data_dir):
        for name in sorted(files):
            ext = os.path.splitext(name)[1].lower()
            if ext in DATA_EXTENSIONS:
                abs_path = os.path.join(root, name)
                found.append((os.path.relpath(abs_path, data_dir), abs_path, DATA_EXTENSIONS[ext]))
    return sorted(found)


def _inspect(rel_path, abs_path, engine, stride, receipts):
    entry = {
        "path": rel_path,
        "sizeBytes": os.path.getsize(abs_path),
        "mtimeIso": _mtime_iso(abs_path),
        "engine": engine,
        "readable": False,
        "error": None,
        "usableAsWindSource": False,
        "unusableReasons": [],
    }

    try:
        ds = xr.open_dataset(abs_path, engine=engine) if engine == "cfgrib" else xr.open_dataset(abs_path)
    except Exception as e:
        hint = ""
        if engine == "cfgrib":
            hint = " -- GRIB support needs the cfgrib engine, which is not in requirements.txt (pip install cfgrib)"
        entry["error"] = f"{type(e).__name__}: {e}{hint}"
        entry["unusableReasons"] = ["file could not be opened"]
        return entry

    try:
        entry["readable"] = True
        entry["dataVars"] = sorted(str(v) for v in ds.data_vars)
        entry["dims"] = {str(k): int(v) for k, v in ds.sizes.items()}

        names, reasons = describe_wind_source(ds)
        entry["unusableReasons"] = reasons
        entry["usableAsWindSource"] = names is not None
        entry["wind"] = names

        provenance, _hash = _provenance(ds.attrs, rel_path, receipts)
        entry["provenance"] = provenance

        if names is not None:
            entry["time"] = _describe_time(ds, names["timeDim"])
            levels = np.asarray(ds[names["levelDim"]].values, dtype=float).ravel()
            entry["levels"] = {
                "count": len(levels),
                "units": str(ds[names["levelDim"]].attrs.get("units", "hPa")),
                "values": [float(v) for v in levels],
            }
            entry["grid"] = _describe_grid(ds, names["latDim"], names["lonDim"])
            entry["payload"] = _describe_payload(len(levels), entry["grid"], stride)
    finally:
        ds.close()

    return entry


# ---------------------------------------------------------------------------
# Human-readable summary
# ---------------------------------------------------------------------------
def _print_summary(catalog):
    print(f"\n{len(catalog['files'])} data file(s) in {catalog['dataDir']}/\n")
    for f in catalog["files"]:
        size_gb = f["sizeBytes"] / (1024**3)
        mark = "OK " if f["usableAsWindSource"] else "-- "
        print(f"{mark}{f['path']}  ({size_gb:.2f} GB)")

        if not f["readable"]:
            print(f"      unreadable: {f['error']}")
            continue

        if f["usableAsWindSource"]:
            t, lv, g = f["time"], f["levels"], f["grid"]
            span_bits = []
            for s in t["spans"]:
                step = f"{s['stepSeconds'] / 3600:g}h" if s["stepSeconds"] else "single"
                span_bits.append(f"{s['startIso']} -> {s['endIso']} ({s['count']} @ {step})")
            print(f"      time:    {t['count']} step(s); " + "; ".join(span_bits))
            print(f"      levels:  {lv['count']} ({lv['values'][0]:g} -> {lv['values'][-1]:g} {lv['units']})")
            print(
                f"      grid:    {g['ny']}x{g['nx']} @ {g['dLat']:g}deg, "
                f"lat {g['latFirst']:g}->{g['latLast']:g}, lon {g['lonFirst']:g}->{g['lonLast']:g}"
                + ("" if g["uniform"] else "  [NON-UNIFORM -- unsupported]")
            )
            p = f["payload"]
            print(
                f"      payload: stride {p['spatialStride']} -> {p['stridedNy']}x{p['stridedNx']}, "
                f"~{p['estimatedJsonMbPerTimeStep']:g} MB JSON / "
                f"~{p['estimatedF32MbPerTimeStep']:g} MB f32 per time step"
            )
        else:
            for reason in f["unusableReasons"]:
                print(f"      not a wind source: {reason}")

        prov = f.get("provenance") or {}
        if prov.get("receiptFile"):
            print(f"      receipt: {prov['receiptFile']} ({prov['collectionId']})")
        elif f["usableAsWindSource"]:
            print("      receipt: none -- keep the Copernicus receipt-*.txt next to downloads")

    if catalog["receiptsWithoutData"]:
        print("\nReceipts with no matching data file on disk:")
        for r in catalog["receiptsWithoutData"]:
            detail = r.get("collectionId") or r.get("error") or "?"
            print(f"  {r['file']}  ({detail})")

    for note in catalog["notes"]:
        print(f"\nnote: {note}")

    usable = [f["path"] for f in catalog["files"] if f["usableAsWindSource"]]
    print()
    if usable:
        print("To serve one of these, set \"file\" in wind_source.json:")
        for path in usable:
            print(f'    "file": "{path}"')
    else:
        print("No usable wind source here — the server will serve synthetic wind.")
        print("See README.md for how to acquire ERA5 pressure-level data.")


# ---------------------------------------------------------------------------
# Entry point
# ---------------------------------------------------------------------------
def build_catalog(data_dir, stride):
    receipts, unmatched_receipts = _load_receipts(data_dir)
    found = _find_data_files(data_dir)

    files, notes = [], []
    for rel_path, abs_path, engine in found:
        try:
            files.append(_inspect(rel_path, abs_path, engine, stride, receipts))
        except Exception:
            # One malformed file must never abort the whole inventory -- the
            # point of the catalog is to tell you about everything it found.
            notes.append(f"{rel_path}: inspection failed, see traceback above")
            traceback.print_exc()

    for f in files:
        if not f["readable"]:
            notes.append(f"{f['path']}: {f['error']}")

    matched_receipt_files = {
        (f.get("provenance") or {}).get("receiptFile") for f in files
    }
    receipts_without_data = [
        {"file": r["receiptFile"], "collectionId": r["collectionId"], "hash": r["hash"]}
        for r in receipts.values()
        if r["receiptFile"] not in matched_receipt_files
    ] + unmatched_receipts

    return {
        "catalogVersion": CATALOG_VERSION,
        "generatedAt": dt.datetime.now(dt.timezone.utc).replace(microsecond=0).isoformat().replace("+00:00", "Z"),
        "generatedBy": "catalog_data.py",
        "dataDir": data_dir,
        "files": files,
        "receiptsWithoutData": receipts_without_data,
        "notes": notes,
    }


def main(argv=None):
    here = os.path.dirname(os.path.abspath(__file__))
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--data-dir", default=os.path.join(here, "data"), help="directory to scan (default: ./data)")
    parser.add_argument("--out", default=None, help="catalog path (default: <data-dir>/catalog.json)")
    parser.add_argument("--stride", type=int, default=2,
                        help="spatial stride to estimate payload size for (default: 2, matching the backend)")
    parser.add_argument("--print", "--dry-run", dest="dry_run", action="store_true",
                        help="print the summary without writing the catalog")
    args = parser.parse_args(argv)

    if not os.path.isdir(args.data_dir):
        raise SystemExit(f"No data directory at {args.data_dir}. Create it and put your ERA5 files there.")

    catalog = build_catalog(args.data_dir, args.stride)
    _print_summary(catalog)

    if args.dry_run:
        print("\n--print given, catalog not written.")
        return 0

    out = args.out or os.path.join(args.data_dir, "catalog.json")
    with open(out, "w") as fh:
        json.dump(catalog, fh, indent=2)
        fh.write("\n")
    print(f"\nwrote {out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
