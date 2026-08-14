import type { TimelineStream } from '../types';

export function calculateDirectTime(stream: TimelineStream): number {
  return stream.focus_intervals.reduce((sum, interval) => {
    return (
      sum +
      (new Date(interval.end).getTime() - new Date(interval.start).getTime())
    );
  }, 0);
}

export function getVisibleStreams(
  streams: TimelineStream[],
  maxColumns: number,
  expanded: boolean,
): {
  visible: TimelineStream[];
  hiddenCount: number;
  aggregate?: TimelineStream;
} {
  // Sort by direct time descending
  const sorted = [...streams].sort(
    (a, b) => calculateDirectTime(b) - calculateDirectTime(a),
  );

  if (expanded || sorted.length <= maxColumns) {
    return { visible: sorted, hiddenCount: 0 };
  }

  const visible = sorted.slice(0, maxColumns - 1);
  const hidden = sorted.slice(maxColumns - 1);

  // Create aggregate stream
  const aggregate: TimelineStream = {
    stream: {
      id: 'aggregate',
      name: `+${hidden.length} more`,
      slug: null,
      description: null,
      color: 'var(--color-text-muted)',
      created_at: '',
      updated_at: '',
      time_direct_ms: 0,
      time_delegated_ms: 0,
      first_event_at: null,
      last_event_at: null,
      needs_recompute: false,
    },
    events: hidden.flatMap((s) => s.events),
    focus_intervals: hidden.flatMap((s) => s.focus_intervals),
    delegated_intervals: hidden.flatMap((s) => s.delegated_intervals),
  };

  return {
    visible: [...visible, aggregate],
    hiddenCount: hidden.length,
    aggregate,
  };
}
