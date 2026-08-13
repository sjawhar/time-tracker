import { describe, expect, it } from 'vitest';
import type { TimelineStream } from '../types';
import { calculateDirectTime, getVisibleStreams } from './columns';

describe('columns', () => {
  const makeStream = (id: string, durationMs: number): TimelineStream => ({
    stream: {
      id,
      name: `Stream ${id}`,
      slug: null,
      description: null,
      color: null,
      created_at: '',
      updated_at: '',
      time_direct_ms: 0,
      time_delegated_ms: 0,
      first_event_at: null,
      last_event_at: null,
      needs_recompute: false,
    },
    events: [],
    focus_intervals: [
      {
        start: new Date(0).toISOString(),
        end: new Date(durationMs).toISOString(),
      },
    ],
    delegated_intervals: [],
  });

  it('calculates direct time correctly', () => {
    const stream = makeStream('1', 5000);
    expect(calculateDirectTime(stream)).toBe(5000);
  });

  it('returns all streams if under maxColumns', () => {
    const streams = [makeStream('1', 1000), makeStream('2', 2000)];
    const result = getVisibleStreams(streams, 15, false);
    expect(result.visible.length).toBe(2);
    expect(result.hiddenCount).toBe(0);
    expect(result.visible[0].stream.id).toBe('2'); // Sorted by duration
  });

  it('caps columns and creates aggregate stream', () => {
    const streams = Array.from({ length: 20 }, (_, i) =>
      makeStream(`${i}`, i * 1000),
    );
    const result = getVisibleStreams(streams, 15, false);

    expect(result.visible.length).toBe(15);
    expect(result.hiddenCount).toBe(6);
    expect(result.aggregate).toBeDefined();
    expect(result.aggregate?.stream.name).toBe('+6 more');
    expect(result.visible[14].stream.id).toBe('aggregate');
  });

  it('returns all streams if expanded', () => {
    const streams = Array.from({ length: 20 }, (_, i) =>
      makeStream(`${i}`, i * 1000),
    );
    const result = getVisibleStreams(streams, 15, true);

    expect(result.visible.length).toBe(20);
    expect(result.hiddenCount).toBe(0);
    expect(result.aggregate).toBeUndefined();
  });
});
