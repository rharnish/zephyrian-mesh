import { describe, it, expect } from 'vitest';
import { fmtLon, fmtLat } from './inspectorPanel.js';

// These match the convention the tower labels already use (towerModel.js), so
// a balloon and a tower at the same place read identically.
describe('fmtLon', () => {
  it('uses E/W rather than a sign', () => {
    expect(fmtLon(82.32)).toBe('82.320° E');
    expect(fmtLon(-82.32)).toBe('82.320° W');
  });

  it('pads to three decimals', () => {
    expect(fmtLon(5)).toBe('5.000° E');
  });

  it('treats the prime meridian as east', () => {
    // 0 is not negative, so it takes the E branch — worth pinning because the
    // sign test is `< 0`, not `<= 0`.
    expect(fmtLon(0)).toBe('0.000° E');
  });

  it('handles the antimeridian', () => {
    expect(fmtLon(180)).toBe('180.000° E');
    expect(fmtLon(-180)).toBe('180.000° W');
  });
});

describe('fmtLat', () => {
  it('uses N/S rather than a sign', () => {
    expect(fmtLat(29.65)).toBe('29.650° N');
    expect(fmtLat(-29.65)).toBe('29.650° S');
  });

  it('treats the equator as north', () => {
    expect(fmtLat(0)).toBe('0.000° N');
  });

  it('handles the poles', () => {
    expect(fmtLat(90)).toBe('90.000° N');
    expect(fmtLat(-90)).toBe('90.000° S');
  });
});
