import * as d3TimeFormat from 'd3-time-format';
import type { TimelineEvent } from '../types';
import type { HitTarget } from './hit-test';

const formatTime = d3TimeFormat.timeFormat('%H:%M:%S');
const formatDuration = (ms: number) => {
  const totalSeconds = Math.floor(ms / 1000);
  const hours = Math.floor(totalSeconds / 3600);
  const minutes = Math.floor((totalSeconds % 3600) / 60);
  const seconds = totalSeconds % 60;

  if (hours > 0) {
    return `${hours}h ${minutes}m`;
  }
  if (minutes > 0) {
    return `${minutes}m ${seconds}s`;
  }
  return `${seconds}s`;
};

export function buildTooltipContent(target: HitTarget): string {
  const streamName =
    target.stream.name || target.stream.slug || 'Unnamed Stream';

  if (target.type === 'event') {
    const time = formatTime(new Date(target.event.timestamp));
    const KIND_LABELS: Record<TimelineEvent['kind'], string> = {
      user_message: 'User Message',
      subagent_start: 'Subagent Start',
      session_start: 'Session Start',
      session_end: 'Session End',
    };
    const kindLabel = KIND_LABELS[target.event.kind];

    let html = `
      <div class="font-medium text-sm text-[var(--color-text-base)]">${streamName}</div>
      <div class="text-xs text-[var(--color-text-muted)] mt-1">${kindLabel}</div>
      <div class="text-xs text-[var(--color-text-muted)]">${time}</div>
    `;

    if (target.event.session_id) {
      html += `<div class="text-xs text-[var(--color-text-muted)] font-mono mt-1">${target.event.session_id}</div>`;
    }

    if (target.event.todo_linked) {
      html += `<div class="text-xs text-[var(--color-status-green)] mt-1 flex items-center gap-1">
        <span class="w-2 h-2 rounded-full bg-[var(--color-status-green)] inline-block"></span>
        Linked to Todo
      </div>`;
    }

    return html;
  } else {
    const start = new Date(target.interval.start);
    const end = new Date(target.interval.end);
    const durationMs = end.getTime() - start.getTime();

    const typeLabel = target.isDelegated ? 'Delegated Time' : 'Direct Focus';

    return `
      <div class="font-medium text-sm text-[var(--color-text-base)]">${streamName}</div>
      <div class="text-xs text-[var(--color-text-muted)] mt-1">${typeLabel}</div>
      <div class="text-xs text-[var(--color-text-muted)]">${formatTime(start)} - ${formatTime(end)}</div>
      <div class="text-xs font-medium text-[var(--color-text-base)] mt-1">${formatDuration(durationMs)}</div>
    `;
  }
}
