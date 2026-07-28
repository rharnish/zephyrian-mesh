import { describe, it, expect } from 'vitest';
import { balloonIconIndexForAltitude } from './balloonIcon.js';
import { BALLOON_MIN_ALT, BALLOON_MAX_ALT } from './config.js';

describe('balloonIconIndexForAltitude', () => {
  it('maps the altitude range onto the full glyph range', () => {
    expect(balloonIconIndexForAltitude(BALLOON_MIN_ALT)).toBe(0);
    expect(balloonIconIndexForAltitude(BALLOON_MAX_ALT)).toBe(9);
  });

  it('puts the midpoint in the middle', () => {
    const mid = (BALLOON_MIN_ALT + BALLOON_MAX_ALT) / 2;
    expect(balloonIconIndexForAltitude(mid)).toBe(5); // 0.5 * 9 = 4.5, rounds up
  });

  it('clamps outside the range rather than indexing past the array', () => {
    // Balloons are server-owned; nothing here guarantees the altitude stays
    // inside the configured band, and an unclamped index returns undefined.
    expect(balloonIconIndexForAltitude(BALLOON_MIN_ALT - 50000)).toBe(0);
    expect(balloonIconIndexForAltitude(BALLOON_MAX_ALT + 50000)).toBe(9);
    expect(balloonIconIndexForAltitude(-1)).toBe(0);
  });

  it('never returns an out-of-bounds index across the whole band', () => {
    for (let alt = BALLOON_MIN_ALT - 5000; alt <= BALLOON_MAX_ALT + 5000; alt += 250) {
      const i = balloonIconIndexForAltitude(alt);
      expect(Number.isInteger(i)).toBe(true);
      expect(i).toBeGreaterThanOrEqual(0);
      expect(i).toBeLessThanOrEqual(9);
    }
  });

  it('increases monotonically with altitude', () => {
    let previous = -1;
    for (let alt = BALLOON_MIN_ALT; alt <= BALLOON_MAX_ALT; alt += 100) {
      const i = balloonIconIndexForAltitude(alt);
      expect(i).toBeGreaterThanOrEqual(previous);
      previous = i;
    }
  });
});
