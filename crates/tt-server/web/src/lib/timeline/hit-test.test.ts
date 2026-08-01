import { describe, expect, it } from 'vitest';
import type { TimelineEvent, TimelineStream } from '../types';
import { TimelineHitTester } from './hit-test';

describe('TimelineHitTester', () => {
  const mockStream = {
    id: '1',
    name: 'Test Stream',
  } as TimelineStream['stream'];

  it('finds nearest event within radius', () => {
    const tester = new TimelineHitTester();

    const event1 = {
      timestamp: '2023-01-01T10:00:00Z',
      kind: 'user_message',
    } as TimelineEvent;
    const event2 = {
      timestamp: '2023-01-01T11:00:00Z',
      kind: 'user_message',
    } as TimelineEvent;

    tester.addEvent(event1, mockStream, 100, 100);
    tester.addEvent(event2, mockStream, 200, 200);
    tester.build();

    // Exact hit
    const hit1 = tester.find(100, 100, 10);
    expect(hit1?.type).toBe('event');
    if (hit1?.type === 'event') expect(hit1.event).toBe(event1);

    // Near hit within radius
    const hit2 = tester.find(105, 105, 10);
    expect(hit2?.type).toBe('event');
    if (hit2?.type === 'event') expect(hit2.event).toBe(event1);

    // Miss outside radius
    const miss = tester.find(150, 150, 10);
    expect(miss).toBeNull();
  });

  it('finds interval when point is inside', () => {
    const tester = new TimelineHitTester();

    const interval = {
      start: '2023-01-01T10:00:00Z',
      end: '2023-01-01T11:00:00Z',
    };

    // x: 100, width: 50, yStart: 100, yEnd: 200
    tester.addInterval(interval, mockStream, false, 100, 100, 200, 50);
    tester.build();

    // Inside
    const hit = tester.find(125, 150, 10);
    expect(hit?.type).toBe('interval');
    if (hit?.type === 'interval') expect(hit.interval).toBe(interval);

    // Outside X
    expect(tester.find(90, 150, 10)).toBeNull();
    expect(tester.find(160, 150, 10)).toBeNull();

    // Outside Y
    expect(tester.find(125, 90, 10)).toBeNull();
    expect(tester.find(125, 210, 10)).toBeNull();
  });

  it('prefers event over interval if both match', () => {
    const tester = new TimelineHitTester();

    const event = {
      timestamp: '2023-01-01T10:30:00Z',
      kind: 'user_message',
    } as TimelineEvent;
    const interval = {
      start: '2023-01-01T10:00:00Z',
      end: '2023-01-01T11:00:00Z',
    };

    tester.addInterval(interval, mockStream, false, 100, 100, 200, 50);
    tester.addEvent(event, mockStream, 125, 150);
    tester.build();

    const hit = tester.find(125, 150, 10);
    expect(hit?.type).toBe('event');
  });
});
