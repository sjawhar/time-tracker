import type { IdleGap } from '../types';

export class PiecewiseTimeScale {
  private domainStart: number;
  private domainEnd: number;
  private rangeTop: number;
  private rangeBottom: number;
  private gaps: (IdleGap & { startMs: number; endMs: number })[];
  private expandedGaps: Set<number>;
  private foldHeight: number;

  constructor(
    domain: [Date, Date], // [end, start]
    range: [number, number], // [top, bottom]
    gaps: IdleGap[],
    expandedGaps: Set<number>,
    foldHeight: number = 40,
  ) {
    this.domainEnd = domain[0].getTime();
    this.domainStart = domain[1].getTime();
    this.rangeTop = range[0];
    this.rangeBottom = range[1];

    // Only consider gaps that overlap with the domain
    this.gaps = gaps
      .map((g) => ({
        ...g,
        startMs: new Date(g.start).getTime(),
        endMs: new Date(g.end).getTime(),
      }))
      .filter((g) => g.endMs > this.domainStart && g.startMs < this.domainEnd)
      .sort((a, b) => a.startMs - b.startMs);

    this.expandedGaps = expandedGaps;

    // Folds must never claim more than this share of the viewport. A real week can
    // contain 49 gaps; at a flat 40px each that is 1960px of folds inside a ~1740px
    // range, which drives active pixels negative and makes folds swallow the whole
    // timeline. Shrink the per-fold height until the total fits the budget.
    const collapsedCount = this.gaps.filter(
      (_, i) => !expandedGaps.has(i),
    ).length;
    const rangePixels = Math.abs(this.rangeBottom - this.rangeTop);
    const foldBudget = rangePixels * PiecewiseTimeScale.MAX_FOLD_SHARE;
    this.foldHeight =
      collapsedCount > 0
        ? Math.max(
            PiecewiseTimeScale.MIN_FOLD_HEIGHT,
            Math.min(foldHeight, foldBudget / collapsedCount),
          )
        : foldHeight;
  }

  /** Folds collectively may occupy at most this fraction of the vertical range. */
  private static readonly MAX_FOLD_SHARE = 0.35;
  /** Below this a fold is unreadable, so it stops shrinking even if over budget. */
  private static readonly MIN_FOLD_HEIGHT = 6;

  public scale(date: Date | number): number {
    const t = typeof date === 'number' ? date : date.getTime();

    // Clamp t to domain
    const clampedT = Math.max(this.domainStart, Math.min(this.domainEnd, t));

    let activeTimeBefore = 0;
    let foldPixelsBefore = 0;

    let currentTime = this.domainStart;

    for (let i = 0; i < this.gaps.length; i++) {
      const gap = this.gaps[i];
      if (this.expandedGaps.has(i)) continue;

      const gapStart = Math.max(this.domainStart, gap.startMs);
      const gapEnd = Math.min(this.domainEnd, gap.endMs);
      const gapDuration = gapEnd - gapStart;

      if (gapDuration <= 0) continue;

      if (clampedT >= gapEnd) {
        activeTimeBefore += gapStart - currentTime;
        foldPixelsBefore += this.foldHeight;
        currentTime = gapEnd;
      } else if (clampedT > gapStart) {
        activeTimeBefore += gapStart - currentTime;
        const fraction = (clampedT - gapStart) / gapDuration;
        foldPixelsBefore += fraction * this.foldHeight;
        currentTime = clampedT;
        break;
      }
    }

    activeTimeBefore += clampedT - currentTime;

    // Calculate total active time and total fold pixels
    let totalActiveTime = 0;
    let totalFoldPixels = 0;
    let curr = this.domainStart;

    for (let i = 0; i < this.gaps.length; i++) {
      const gap = this.gaps[i];
      if (this.expandedGaps.has(i)) continue;

      const gapStart = Math.max(this.domainStart, gap.startMs);
      const gapEnd = Math.min(this.domainEnd, gap.endMs);
      const gapDuration = gapEnd - gapStart;

      if (gapDuration <= 0) continue;

      totalActiveTime += gapStart - curr;
      totalFoldPixels += this.foldHeight;
      curr = gapEnd;
    }
    totalActiveTime += this.domainEnd - curr;

    const totalPixels = this.rangeBottom - this.rangeTop;
    const activePixels = Math.max(0, totalPixels - totalFoldPixels);

    const scaleFactor =
      totalActiveTime > 0 ? activePixels / totalActiveTime : 0;

    const pixelsFromBottom = activeTimeBefore * scaleFactor + foldPixelsBefore;
    return this.rangeBottom - pixelsFromBottom;
  }

  public invert(y: number): Date {
    // Clamp y to range
    const clampedY = Math.max(this.rangeTop, Math.min(this.rangeBottom, y));
    const pixelsFromBottom = this.rangeBottom - clampedY;

    let totalActiveTime = 0;
    let totalFoldPixels = 0;
    let curr = this.domainStart;

    for (let i = 0; i < this.gaps.length; i++) {
      const gap = this.gaps[i];
      if (this.expandedGaps.has(i)) continue;

      const gapStart = Math.max(this.domainStart, gap.startMs);
      const gapEnd = Math.min(this.domainEnd, gap.endMs);
      const gapDuration = gapEnd - gapStart;

      if (gapDuration <= 0) continue;

      totalActiveTime += gapStart - curr;
      totalFoldPixels += this.foldHeight;
      curr = gapEnd;
    }
    totalActiveTime += this.domainEnd - curr;

    const totalPixels = this.rangeBottom - this.rangeTop;
    const activePixels = Math.max(0, totalPixels - totalFoldPixels);
    const scaleFactor =
      totalActiveTime > 0 ? activePixels / totalActiveTime : 0;

    let currentPixels = 0;
    let currentTime = this.domainStart;

    for (let i = 0; i < this.gaps.length; i++) {
      const gap = this.gaps[i];
      if (this.expandedGaps.has(i)) continue;

      const gapStart = Math.max(this.domainStart, gap.startMs);
      const gapEnd = Math.min(this.domainEnd, gap.endMs);
      const gapDuration = gapEnd - gapStart;

      if (gapDuration <= 0) continue;

      const activeTimeBeforeGap = gapStart - currentTime;
      const activePixelsBeforeGap = activeTimeBeforeGap * scaleFactor;

      if (pixelsFromBottom <= currentPixels + activePixelsBeforeGap) {
        const remainingPixels = pixelsFromBottom - currentPixels;
        const time =
          currentTime + (scaleFactor > 0 ? remainingPixels / scaleFactor : 0);
        return new Date(time);
      }

      currentPixels += activePixelsBeforeGap;
      currentTime = gapStart;

      if (pixelsFromBottom <= currentPixels + this.foldHeight) {
        const remainingPixels = pixelsFromBottom - currentPixels;
        const fraction = remainingPixels / this.foldHeight;
        return new Date(gapStart + fraction * gapDuration);
      }

      currentPixels += this.foldHeight;
      currentTime = gapEnd;
    }

    const remainingPixels = pixelsFromBottom - currentPixels;
    const time =
      currentTime + (scaleFactor > 0 ? remainingPixels / scaleFactor : 0);
    return new Date(time);
  }

  public getTicks(interval: {
    range: (start: Date, end: Date) => Date[];
  }): Date[] {
    // We can just use d3-time intervals on the domain, but we might want to filter out ticks that fall inside collapsed gaps?
    // Actually, d3 scaleTime().ticks() just generates ticks. We can generate them and map them.
    // Let's just return the ticks from the interval between start and end.
    return interval.range(new Date(this.domainStart), new Date(this.domainEnd));
  }

  public getCollapsedGaps(): {
    startMs: number;
    endMs: number;
    yTop: number;
    yBottom: number;
    index: number;
    durationMinutes: number;
  }[] {
    const result = [];
    for (let i = 0; i < this.gaps.length; i++) {
      if (this.expandedGaps.has(i)) continue;
      const gap = this.gaps[i];
      const gapStart = Math.max(this.domainStart, gap.startMs);
      const gapEnd = Math.min(this.domainEnd, gap.endMs);
      if (gapEnd <= gapStart) continue;

      const yBottom = this.scale(gapStart);
      const yTop = this.scale(gapEnd);

      result.push({
        startMs: gapStart,
        endMs: gapEnd,
        yTop,
        yBottom,
        index: i,
        durationMinutes: gap.duration_minutes,
      });
    }
    return result;
  }
}
