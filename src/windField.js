import * as Cesium from 'cesium';

// ---------------------------------------------------------------------------
// Wind field — fetched once from the FastAPI backend, then interpolated
// client-side every tick (3D: bracket by pressure-level altitude, bilinear
// interpolate horizontally on each level, blend between levels).
// ---------------------------------------------------------------------------
export class WindField {
  constructor(header, levels) {
    this.header = header;       // { nx, ny, lo1, la1, lo2, la2, dx, dy }
    this.levels = levels;       // sorted ascending by altitudeM: [{pressureHpa, altitudeM, u_data, v_data}]
  }

  static async fetchFromBackend(url) {
    const res = await fetch(url);
    if (!res.ok) throw new Error(`Wind field fetch failed: ${res.status}`);
    const json = await res.json();
    return new WindField(json.header, json.levels);
  }

  // Normalize a Cesium-style [-180, 180] longitude into the grid's own
  // longitude convention (ERA5 grids are commonly 0-360).
  _normalizeLon(lon) {
    const { lo1, lo2 } = this.header;
    let normalized = lon;
    // If the grid spans roughly 0..360 but we were given a negative lon, shift it.
    if (lo1 >= 0 && lo2 > 180 && normalized < 0) {
      normalized += 360;
    }
    return normalized;
  }

  // Bilinear interpolation of a single level's u/v grid at (lon, lat).
  _sampleLevel(level, lon, lat) {
    const { nx, ny, lo1, la1, dx, dy } = this.header;
    const gLon = this._normalizeLon(lon);

    // Fractional column/row position in the grid.
    const colF = (gLon - lo1) / dx;
    const rowF = (la1 - lat) / dy; // la1 is the max lat (top row), dy positive downward

    const col0 = Cesium.Math.clamp(Math.floor(colF), 0, nx - 2 < 0 ? 0 : nx - 2);
    const row0 = Cesium.Math.clamp(Math.floor(rowF), 0, ny - 2 < 0 ? 0 : ny - 2);
    const col1 = Math.min(col0 + 1, nx - 1);
    const row1 = Math.min(row0 + 1, ny - 1);

    const tx = Cesium.Math.clamp(colF - col0, 0, 1);
    const ty = Cesium.Math.clamp(rowF - row0, 0, 1);

    // u_data/v_data are stored as [row][col] nested arrays (matches np -> tolist()).
    const u00 = level.u_data[row0][col0], u10 = level.u_data[row0][col1];
    const u01 = level.u_data[row1][col0], u11 = level.u_data[row1][col1];
    const v00 = level.v_data[row0][col0], v10 = level.v_data[row0][col1];
    const v01 = level.v_data[row1][col0], v11 = level.v_data[row1][col1];

    const u = (1 - tx) * (1 - ty) * u00 + tx * (1 - ty) * u10 + (1 - tx) * ty * u01 + tx * ty * u11;
    const v = (1 - tx) * (1 - ty) * v00 + tx * (1 - ty) * v10 + (1 - tx) * ty * v01 + tx * ty * v11;
    return { u, v };
  }

  // Public: wind vector at (lon, lat, altM), blended between the two
  // pressure levels that bracket altM.
  sample(lon, lat, altM) {
    const levels = this.levels;
    if (!levels || levels.length === 0) return { u: 0, v: 0 };

    if (altM <= levels[0].altitudeM) return this._sampleLevel(levels[0], lon, lat);
    if (altM >= levels[levels.length - 1].altitudeM) {
      return this._sampleLevel(levels[levels.length - 1], lon, lat);
    }

    // Walk up until we find the bracketing pair (levels list is short, ~37 entries).
    let lower = levels[0], upper = levels[levels.length - 1];
    for (let i = 0; i < levels.length - 1; i++) {
      if (levels[i].altitudeM <= altM && levels[i + 1].altitudeM >= altM) {
        lower = levels[i];
        upper = levels[i + 1];
        break;
      }
    }

    const wLower = this._sampleLevel(lower, lon, lat);
    const wUpper = this._sampleLevel(upper, lon, lat);
    const span = upper.altitudeM - lower.altitudeM;
    const t = span > 0 ? (altM - lower.altitudeM) / span : 0;

    return {
      u: wLower.u + (wUpper.u - wLower.u) * t,
      v: wLower.v + (wUpper.v - wLower.v) * t,
    };
  }
}
