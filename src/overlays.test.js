import { describe, it, expect } from 'vitest';
import {
  beliefKey,
  deliveryKey,
  bundleOutcome,
  deliveryMix,
  commsAckLabel,
  BELIEF_CSS,
  BELIEF_LEGEND,
  BELIEF_VERDICT,
  DELIVERY_CSS,
  DELIVERY_LEGEND,
  COMMS_OUTCOME_CSS,
  MUTED_CSS,
} from './overlays.js';

describe('beliefKey', () => {
  // The whole point of the overlay is the two off-diagonal cases: a balloon
  // can believe a route it has lost, and can have one it hasn't heard about.
  it('separates belief from truth across all four combinations', () => {
    expect(beliefKey({ believedHops: 3, grounded: true })).toBe('ok');
    expect(beliefKey({ believedHops: 3, grounded: false })).toBe('stale');
    expect(beliefKey({ believedHops: null, grounded: true })).toBe('unaware');
    expect(beliefKey({ believedHops: null, grounded: false })).toBe('none');
  });

  it('treats zero hops as a real belief, not a missing one', () => {
    // A balloon adjacent to a tower believes at hop 0; `0` is falsy, so this
    // is the case a truthiness check would silently get wrong.
    expect(beliefKey({ believedHops: 0, grounded: true })).toBe('ok');
    expect(beliefKey({ believedHops: 0, grounded: false })).toBe('stale');
  });

  it('treats an absent believedHops the same as an explicit null', () => {
    expect(beliefKey({ grounded: true })).toBe('unaware');
    expect(beliefKey({ grounded: false })).toBe('none');
  });
});

describe('deliveryKey', () => {
  it('maps each channel to itself', () => {
    expect(deliveryKey({ lastChannel: 'radio' })).toBe('radio');
    expect(deliveryKey({ lastChannel: 'satellite' })).toBe('satellite');
  });

  it('falls back to none when nothing has resolved yet', () => {
    expect(deliveryKey({ lastChannel: null })).toBe('none');
    expect(deliveryKey({})).toBe('none');
  });
});

describe('deliveryMix', () => {
  const of = (...channels) => channels.map((lastChannel) => ({ lastChannel }));

  it('splits the field across the three buckets', () => {
    const mix = deliveryMix(of('radio', 'radio', 'satellite', null));
    expect(mix).toEqual({ radio: 50, satellite: 25, none: 25 });
  });

  it('counts a missing or null channel as unresolved', () => {
    expect(deliveryMix([{ lastChannel: null }, {}])).toEqual({ radio: 0, satellite: 0, none: 100 });
  });

  it('returns zeros for an empty field rather than dividing by zero', () => {
    expect(deliveryMix([])).toEqual({ radio: 0, satellite: 0, none: 0 });
  });

  it('sums to 100 for any field', () => {
    const mix = deliveryMix(of('radio', 'satellite', null, 'radio', 'radio', 'satellite', null));
    expect(mix.radio + mix.satellite + mix.none).toBeCloseTo(100);
  });

  it('does not drop a channel it does not recognise', () => {
    // A new server-side channel must not make the legend silently under-count;
    // it lands in "nothing resolved yet" until the client learns about it.
    const mix = deliveryMix(of('radio', 'laser'));
    expect(mix.radio + mix.satellite + mix.none).toBeCloseTo(100);
    expect(mix.none).toBe(50);
  });
});

describe('bundleOutcome', () => {
  it('reports satellite delivery regardless of ack state', () => {
    // Satellite delivery is silent to the origin, so its ack state is
    // meaningless and must not be allowed to classify the bundle.
    expect(bundleOutcome({ channel: 'satellite', state: 'timedOut' })).toBe('satellite');
    expect(bundleOutcome({ channel: 'satellite', state: 'acked' })).toBe('satellite');
  });

  it('reports a completed round trip', () => {
    expect(bundleOutcome({ channel: 'radio', state: 'acked' })).toBe('acked');
  });

  it('reports an ack that died partway back', () => {
    expect(bundleOutcome({ channel: 'radio', state: 'timedOut', ackHopsCompleted: 2 })).toBe('ackDied');
  });

  it('counts a zero-hop ack as died, not as never delivered', () => {
    // 0 is falsy but meaningful: the bundle reached a tower and the ack died
    // on its very first hop back.
    expect(bundleOutcome({ channel: 'radio', state: 'timedOut', ackHopsCompleted: 0 })).toBe('ackDied');
  });

  it('reports a bundle that never reached a tower', () => {
    expect(bundleOutcome({ channel: 'radio', state: 'timedOut', ackHopsCompleted: null })).toBe('droppedInMesh');
    expect(bundleOutcome({ channel: null, state: 'timedOut' })).toBe('droppedInMesh');
  });
});

describe('commsAckLabel', () => {
  it('labels a completed ack', () => {
    expect(commsAckLabel('acked', 3, 3)).toEqual({ text: 'acked', css: COMMS_OUTCOME_CSS.acked });
  });

  it('labels an unresolved record', () => {
    expect(commsAckLabel('pending', null, null)).toEqual({ text: 'pending', css: MUTED_CSS });
  });

  it('shows how far a died ack got', () => {
    expect(commsAckLabel('timedOut', 1, 4)).toEqual({ text: 'died @ 1/4', css: COMMS_OUTCOME_CSS.ackDied });
    expect(commsAckLabel('timedOut', 0, 2)).toEqual({ text: 'died @ 0/2', css: COMMS_OUTCOME_CSS.ackDied });
  });

  it('falls back to a bare timeout when no hop count was recorded', () => {
    expect(commsAckLabel('timedOut', null, 4)).toEqual({ text: 'timed out', css: MUTED_CSS });
  });
});

describe('palette', () => {
  it('keeps every belief key covered by color, legend, and verdict', () => {
    // These three tables are read by three different call sites; a key added
    // to one and missed in another shows up as `undefined` in the UI.
    const keys = Object.keys(BELIEF_CSS);
    expect(Object.keys(BELIEF_LEGEND).sort()).toEqual(keys.slice().sort());
    expect(Object.keys(BELIEF_VERDICT).sort()).toEqual(keys.slice().sort());
  });

  it('keeps every delivery key covered by color and legend', () => {
    expect(Object.keys(DELIVERY_LEGEND).sort()).toEqual(Object.keys(DELIVERY_CSS).sort());
  });

  it('ties the bundle-outcome colors to the palettes they echo', () => {
    expect(COMMS_OUTCOME_CSS.acked).toBe(BELIEF_CSS.ok);
    expect(COMMS_OUTCOME_CSS.ackDied).toBe(BELIEF_CSS.stale);
    expect(COMMS_OUTCOME_CSS.satellite).toBe(DELIVERY_CSS.satellite);
  });

  it('every color is a valid hex string', () => {
    const all = [...Object.values(BELIEF_CSS), ...Object.values(DELIVERY_CSS), ...Object.values(COMMS_OUTCOME_CSS)];
    for (const css of all) expect(css).toMatch(/^#[0-9a-f]{6}$/);
  });
});
