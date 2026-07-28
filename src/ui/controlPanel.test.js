import { describe, it, expect, vi, afterEach } from 'vitest';
import { PendingRequest, PENDING_REQUEST_TIMEOUT_MS } from './controlPanel.js';

const exact = () => new PendingRequest((a, b) => a === b);
const approx = () => new PendingRequest((a, b) => Math.abs(a - b) < 1e-9);

afterEach(() => vi.useRealTimers());

describe('PendingRequest', () => {
  it('follows the server when nothing is outstanding', () => {
    const r = exact();
    expect(r.shouldIgnore(42)).toBe(false);
  });

  it('ignores stale values until the request is confirmed', () => {
    // This is the bounce: after asking for 1200, snapshots already in flight
    // still say 1600. Applying one of those is what snapped the slider back.
    const r = exact();
    r.request(1200);
    expect(r.shouldIgnore(1600)).toBe(true);
    expect(r.shouldIgnore(1600)).toBe(true);
    expect(r.shouldIgnore(1600)).toBe(true);
  });

  it('resumes following the server once confirmed', () => {
    const r = exact();
    r.request(1200);
    expect(r.shouldIgnore(1600)).toBe(true);
    expect(r.shouldIgnore(1200)).toBe(false); // the confirmation itself
    // Another tab moves the slider — that must land, not be ignored.
    expect(r.shouldIgnore(300)).toBe(false);
  });

  it('does not ignore an unrelated later change once settled', () => {
    const r = exact();
    r.request(1200);
    r.shouldIgnore(1200);
    expect(r.shouldIgnore(1201)).toBe(false);
  });

  it('treats a superseding request as the one to wait for', () => {
    // Dragging twice quickly: the second release replaces the first, so only
    // the newest value counts as confirmation.
    const r = exact();
    r.request(1200);
    r.request(800);
    expect(r.shouldIgnore(1200)).toBe(true); // the first request's echo is now stale too
    expect(r.shouldIgnore(800)).toBe(false);
  });

  it('gives up after the timeout so a lost request cannot wedge the control', () => {
    vi.useFakeTimers();
    const r = exact();
    r.request(1200);
    expect(r.shouldIgnore(1600)).toBe(true);
    vi.advanceTimersByTime(PENDING_REQUEST_TIMEOUT_MS + 1);
    // The server never confirmed — accept its truth rather than ignoring for ever.
    expect(r.shouldIgnore(1600)).toBe(false);
    expect(r.shouldIgnore(1600)).toBe(false);
  });

  it('still ignores right up to the timeout', () => {
    vi.useFakeTimers();
    const r = exact();
    r.request(1200);
    vi.advanceTimersByTime(PENDING_REQUEST_TIMEOUT_MS - 1);
    expect(r.shouldIgnore(1600)).toBe(true);
  });

  it('confirms floats that survive a JSON round trip', () => {
    const r = approx();
    r.request(3.75);
    expect(r.shouldIgnore(JSON.parse(JSON.stringify(3.75)))).toBe(false);
  });

  it('confirms floats carrying representation error', () => {
    // The horizon slider steps by 0.05, so a value can arrive back a few ulps
    // off. An exact === would never confirm and the control would sit ignoring
    // snapshots until the timeout.
    const r = approx();
    r.request(3.75);
    expect(r.shouldIgnore(3.75 + 1e-15)).toBe(false);
  });

  it('does not confirm a float that is genuinely a different step', () => {
    const r = approx();
    r.request(3.75);
    expect(r.shouldIgnore(3.7)).toBe(true);
  });

  it('treats 0 as a real requested value', () => {
    // `null` means "nothing outstanding", so a falsy request must not be
    // mistaken for one.
    const r = exact();
    r.request(0);
    expect(r.shouldIgnore(500)).toBe(true);
    expect(r.shouldIgnore(0)).toBe(false);
  });
});
