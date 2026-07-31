import { describe, it, expect } from 'vitest';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { BALLOON_MIN_ALT, BALLOON_MAX_ALT, EARTH_RADIUS } from './config.js';

// sim-server/src/config.rs re-derives these instead of reading config.js
// (it's a separate Rust process — see sim-server/README.md), so nothing
// forces the two to agree. This test parses the Rust source as the source
// of truth and fails loudly if a value here drifts from it, instead of
// leaving that to be discovered by a balloon rendering at the wrong altitude.
const rustConfigPath = fileURLToPath(
  new URL('../sim-server/src/config.rs', import.meta.url),
);
const rustConfig = readFileSync(rustConfigPath, 'utf8');

function rustConstant(name) {
  const match = rustConfig.match(new RegExp(`pub const ${name}: f64 = ([\\d_.]+)`));
  if (!match) {
    throw new Error(`${name} not found in sim-server/src/config.rs — did it get renamed?`);
  }
  return Number(match[1].replace(/_/g, ''));
}

describe('constants shared with sim-server/src/config.rs', () => {
  it('BALLOON_MIN_ALT matches', () => {
    expect(BALLOON_MIN_ALT).toBe(rustConstant('BALLOON_MIN_ALT'));
  });

  it('BALLOON_MAX_ALT matches', () => {
    expect(BALLOON_MAX_ALT).toBe(rustConstant('BALLOON_MAX_ALT'));
  });

  it('EARTH_RADIUS matches EARTH_RADIUS_M', () => {
    expect(EARTH_RADIUS).toBe(rustConstant('EARTH_RADIUS_M'));
  });
});
