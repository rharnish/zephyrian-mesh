#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
Shared knowledge about what a usable wind source looks like.

Imported by both `wind_backend.py` (which reads the data) and
`catalog_data.py` (which decides whether a file *could* be read). Keeping the
sniffing in one place is the point: "is this file usable" and "how do I read
this file" can't drift apart into two subtly different answers.

Deliberately has no FastAPI or server imports so the catalog script stays
standalone.
"""

# ERA5 pressure-level variable names are usually 'u' and 'v'. Some exports
# use 'u_component_of_wind' / 'v_component_of_wind'. We try both.
U_VAR_CANDIDATES = ["u", "u_component_of_wind"]
V_VAR_CANDIDATES = ["v", "v_component_of_wind"]
LEVEL_DIM_CANDIDATES = ["pressure_level", "level", "isobaricInhPa"]
TIME_DIM_CANDIDATES = ["valid_time", "time"]
LAT_DIM_CANDIDATES = ["latitude", "lat"]
LON_DIM_CANDIDATES = ["longitude", "lon"]


def pick_var(ds, candidates, kind):
    for name in candidates:
        if name in ds.variables:
            return name
    raise RuntimeError(
        f"Could not find a {kind} variable in the dataset. "
        f"Looked for: {candidates}. Available: {list(ds.variables)}"
    )


def pick_dim(ds, candidates, kind):
    for name in candidates:
        if name in ds.dims:
            return name
    raise RuntimeError(
        f"Could not find a {kind} dimension in the dataset. "
        f"Looked for: {candidates}. Available: {list(ds.dims)}"
    )


def _first_present(ds, candidates, in_vars=False):
    """Non-raising variant of pick_var/pick_dim, for use when we want to
    collect *all* the reasons a file is unusable rather than stop at the
    first one."""
    haystack = ds.variables if in_vars else ds.dims
    for name in candidates:
        if name in haystack:
            return name
    return None


def describe_wind_source(ds):
    """Resolve the variable/dimension names this dataset would be read
    through, or explain why it can't be.

    Returns ``(names, reasons)``:

    - ``names`` is a dict with keys ``uVar, vVar, levelDim, timeDim, latDim,
      lonDim`` when the dataset is usable as a wind source, else ``None``.
    - ``reasons`` is a list of human-readable strings -- empty when usable,
      otherwise every problem found (not just the first), because the catalog
      output is more useful when it names all of them at once.

    "Usable" means: u and v are present, a pressure-level / time / lat / lon
    dimension each resolve, and *u and v actually carry all four of those
    dims*. That last check is what rejects ERA5 single-level products, whose
    10m/100m wind variables have no pressure-level dim even when the file
    otherwise looks similar.
    """
    reasons = []

    u_var = _first_present(ds, U_VAR_CANDIDATES, in_vars=True)
    v_var = _first_present(ds, V_VAR_CANDIDATES, in_vars=True)
    if u_var is None:
        reasons.append(f"no u-wind variable (looked for {', '.join(U_VAR_CANDIDATES)})")
    if v_var is None:
        reasons.append(f"no v-wind variable (looked for {', '.join(V_VAR_CANDIDATES)})")

    dims = {}
    for key, candidates, label in (
        ("levelDim", LEVEL_DIM_CANDIDATES, "pressure-level dim"),
        ("timeDim", TIME_DIM_CANDIDATES, "time dim"),
        ("latDim", LAT_DIM_CANDIDATES, "latitude dim"),
        ("lonDim", LON_DIM_CANDIDATES, "longitude dim"),
    ):
        found = _first_present(ds, candidates)
        dims[key] = found
        if found is None:
            reasons.append(f"no {label} (looked for {', '.join(candidates)})")

    # The wind variables must actually be indexed by all four dims. A file can
    # have a pressure_level dim sitting on some *other* variable while u/v are
    # single-level -- that is not a usable pressure-level wind source.
    for var_name, var_label in ((u_var, "u"), (v_var, "v")):
        if var_name is None:
            continue
        var_dims = set(ds[var_name].dims)
        missing = [
            dims[key]
            for key in ("levelDim", "timeDim", "latDim", "lonDim")
            if dims[key] is not None and dims[key] not in var_dims
        ]
        if missing:
            reasons.append(
                f"variable '{var_label}' is not indexed by {', '.join(missing)} "
                f"(it has dims {list(ds[var_name].dims)})"
            )

    if reasons:
        return None, reasons

    return {
        "uVar": u_var,
        "vVar": v_var,
        "levelDim": dims["levelDim"],
        "timeDim": dims["timeDim"],
        "latDim": dims["latDim"],
        "lonDim": dims["lonDim"],
    }, []
