import { describe, it, expect } from 'vitest';
import { parseNodeKey } from './linkLayer.js';

describe('parseNodeKey', () => {
  it('splits a balloon key into kind and id', () => {
    expect(parseNodeKey('b12')).toEqual({ kind: 'b', id: 12 });
  });

  it('splits a tower key into kind and id', () => {
    expect(parseNodeKey('t3')).toEqual({ kind: 't', id: 3 });
  });

  it('handles multi-digit and zero ids', () => {
    // Balloon counts run into the thousands, and id 0 is a real balloon —
    // it must not be confused with a parse failure.
    expect(parseNodeKey('b0')).toEqual({ kind: 'b', id: 0 });
    expect(parseNodeKey('b1999')).toEqual({ kind: 'b', id: 1999 });
  });

  it('round-trips the halves of a pairKey', () => {
    // Edge pairKeys arrive as "a|b" and are split before parsing; this is the
    // shape LinkLayer.sync stores and refreshPositions later resolves.
    const [aKey, bKey] = 'b12|t3'.split('|');
    expect(parseNodeKey(aKey)).toEqual({ kind: 'b', id: 12 });
    expect(parseNodeKey(bKey)).toEqual({ kind: 't', id: 3 });
  });
});
