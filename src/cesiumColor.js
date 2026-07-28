import * as Cesium from 'cesium';

// overlays.js keeps every palette as CSS strings, because the HTML panels
// render them directly and a Cesium.Color would have to be converted back.
// The rendering layers need the other representation, so they convert once at
// module load through here rather than each keeping its own copy of the hexes.
export function toCesiumColors(cssTable) {
  return Object.fromEntries(
    Object.entries(cssTable).map(([key, css]) => [key, Cesium.Color.fromCssColorString(css)])
  );
}
