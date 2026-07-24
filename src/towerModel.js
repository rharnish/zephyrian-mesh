// ---------------------------------------------------------------------------
// TowerModel — the plain data/identity half of a tower (id, position,
// height), with no Cesium entity or canvas rendering dependency. Split out
// from Tower (tower.js) so headless code (e.g. the connectivity-sweep
// experiment) can construct/position towers without pulling in any
// browser/DOM/WebGL machinery at all.
// ---------------------------------------------------------------------------
let towerDisplayCounter = 0;

export class TowerModel {
  constructor(lon, lat, heightM) {
    // Id derived from a running counter (fixed precision on the label
    // below, so two towers at the same rounded coordinates would only
    // collide in their on-map label, not their id).
    this.id = towerDisplayCounter++;
    this.lon = lon;
    this.lat = lat;
    this.heightM = heightM;
    // Separate display label just for the on-map label, e.g. "82.32 W 29.65 N".
    const lonDir = lon < 0 ? 'W' : 'E';
    const latDir = lat < 0 ? 'S' : 'N';
    this.label = `${Math.abs(lon).toFixed(2)} ${lonDir} ${Math.abs(lat).toFixed(2)} ${latDir}`;
  }
}
