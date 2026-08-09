import { describe, expect, it } from 'vitest';
import { PiecewiseTimeScale } from './time-scale';

describe('PiecewiseTimeScale', () => {
  it('scales linearly when there are no gaps', () => {
    const start = new Date('2026-07-24T00:00:00Z');
    const end = new Date('2026-07-24T10:00:00Z');
    const scale = new PiecewiseTimeScale(
      [end, start],
      [0, 1000],
      [],
      new Set(),
    );

    expect(scale.scale(start)).toBe(1000);
    expect(scale.scale(end)).toBe(0);

    const mid = new Date('2026-07-24T05:00:00Z');
    expect(scale.scale(mid)).toBe(500);

    expect(scale.invert(500).getTime()).toBe(mid.getTime());
  });

  it('collapses gaps to foldHeight', () => {
    const start = new Date('2026-07-24T00:00:00Z');
    const end = new Date('2026-07-24T10:00:00Z');

    const gaps = [
      {
        start: '2026-07-24T02:00:00Z',
        end: '2026-07-24T08:00:00Z',
        duration_minutes: 360,
      },
    ];

    const scale = new PiecewiseTimeScale(
      [end, start],
      [0, 1000],
      gaps,
      new Set(),
      40,
    );

    // Total time = 10 hours. Gap = 6 hours. Active time = 4 hours.
    // Total pixels = 1000. Fold pixels = 40. Active pixels = 960.
    // Scale factor = 960 / 4 hours = 240 pixels per hour.

    expect(scale.scale(start)).toBe(1000);
    expect(scale.scale(end)).toBe(0);

    const gapStart = new Date('2026-07-24T02:00:00Z');
    const gapEnd = new Date('2026-07-24T08:00:00Z');

    // gapStart is 2 hours from start. 2 * 240 = 480 pixels from bottom.
    // So y = 1000 - 480 = 520.
    expect(scale.scale(gapStart)).toBe(520);

    // gapEnd is after the gap. The gap takes 40 pixels.
    // So y = 520 - 40 = 480.
    expect(scale.scale(gapEnd)).toBe(480);

    // Mid gap should be halfway through the 40 pixels.
    const midGap = new Date('2026-07-24T05:00:00Z');
    expect(scale.scale(midGap)).toBe(500);

    // Invert tests
    expect(scale.invert(520).getTime()).toBe(gapStart.getTime());
    expect(scale.invert(480).getTime()).toBe(gapEnd.getTime());
    expect(scale.invert(500).getTime()).toBe(midGap.getTime());
  });

  it('ignores expanded gaps', () => {
    const start = new Date('2026-07-24T00:00:00Z');
    const end = new Date('2026-07-24T10:00:00Z');

    const gaps = [
      {
        start: '2026-07-24T02:00:00Z',
        end: '2026-07-24T08:00:00Z',
        duration_minutes: 360,
      },
    ];

    const expanded = new Set([0]);
    const scale = new PiecewiseTimeScale(
      [end, start],
      [0, 1000],
      gaps,
      expanded,
      40,
    );

    // Should behave like linear scale
    const mid = new Date('2026-07-24T05:00:00Z');
    expect(scale.scale(mid)).toBe(500);
  });
});
