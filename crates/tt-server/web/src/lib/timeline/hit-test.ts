import { type Quadtree, quadtree } from 'd3-quadtree';
import type { Interval, TimelineEvent, TimelineStream } from '../types';

export type HitTarget =
  | {
      type: 'event';
      event: TimelineEvent;
      stream: TimelineStream['stream'];
      x: number;
      y: number;
    }
  | {
      type: 'interval';
      interval: Interval;
      stream: TimelineStream['stream'];
      isDelegated: boolean;
      x: number;
      yStart: number;
      yEnd: number;
      width: number;
    };

export class TimelineHitTester {
  private tree: Quadtree<HitTarget>;
  private intervals: HitTarget[] = [];

  constructor() {
    this.tree = quadtree<HitTarget>()
      .x((d) => (d.type === 'event' ? d.x : d.x + d.width / 2))
      .y((d) => (d.type === 'event' ? d.y : (d.yStart + d.yEnd) / 2));
  }

  addEvent(
    event: TimelineEvent,
    stream: TimelineStream['stream'],
    x: number,
    y: number,
  ) {
    this.tree.add({ type: 'event', event, stream, x, y });
  }

  addInterval(
    interval: Interval,
    stream: TimelineStream['stream'],
    isDelegated: boolean,
    x: number,
    yStart: number,
    yEnd: number,
    width: number,
  ) {
    const target: HitTarget = {
      type: 'interval',
      interval,
      stream,
      isDelegated,
      x,
      yStart,
      yEnd,
      width,
    };
    this.intervals.push(target);
  }

  build() {
    this.intervals.sort((a, b) => {
      if (a.type === 'interval' && b.type === 'interval') {
        return a.yStart - b.yStart;
      }
      return 0;
    });
  }

  find(x: number, y: number, radius: number): HitTarget | null {
    const nearestEvent = this.tree.find(x, y, radius);
    if (nearestEvent) {
      return nearestEvent;
    }

    let left = 0;
    let right = this.intervals.length - 1;
    let bestIdx = -1;

    while (left <= right) {
      const mid = Math.floor((left + right) / 2);
      const interval = this.intervals[mid] as Extract<
        HitTarget,
        { type: 'interval' }
      >;

      if (interval.yEnd >= y) {
        bestIdx = mid;
        right = mid - 1;
      } else {
        left = mid + 1;
      }
    }

    if (bestIdx !== -1) {
      for (let i = bestIdx; i < this.intervals.length; i++) {
        const interval = this.intervals[i] as Extract<
          HitTarget,
          { type: 'interval' }
        >;
        if (interval.yStart > y) {
          break;
        }

        if (
          y >= interval.yStart &&
          y <= interval.yEnd &&
          x >= interval.x &&
          x <= interval.x + interval.width
        ) {
          return interval;
        }
      }
    }

    return null;
  }
}
