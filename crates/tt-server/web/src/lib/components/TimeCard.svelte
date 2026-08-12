<script lang="ts">
import type { Report } from '../types';

let {
  report,
  period,
  onPeriodChange,
}: {
  report: Report;
  period: 'day' | 'week';
  onPeriodChange: (p: 'day' | 'week') => void;
} = $props();

function formatDuration(ms: number): string {
  if (ms === 0) return '0m';
  const totalMinutes = Math.floor(ms / 60000);
  const hours = Math.floor(totalMinutes / 60);
  const minutes = totalMinutes % 60;
  if (hours > 0) {
    return `${hours}h ${minutes}m`;
  }
  return `${minutes}m`;
}

function formatLeverage(direct: number, delegated: number): string {
  if (direct === 0) return '∞';
  return `${(delegated / direct).toFixed(1)}x`;
}

let junkStream = $derived(report.streams.find((s) => s.id === 'junk'));
let realStreams = $derived(report.streams.filter((s) => s.id !== 'junk'));

let attendedStreams = $derived(realStreams.filter((s) => s.time_direct_ms > 0));
let unattendedStreams = $derived(
  realStreams.filter((s) => s.time_direct_ms === 0),
);

// Collapse tail: streams under 15m
const MIN_DISPLAY_MS = 15 * 60 * 1000;
let visibleAttended = $derived(
  attendedStreams.filter((s) => s.time_direct_ms >= MIN_DISPLAY_MS),
);
let tailAttended = $derived(
  attendedStreams.filter((s) => s.time_direct_ms < MIN_DISPLAY_MS),
);

let tailDirect = $derived(
  tailAttended.reduce((sum, s) => sum + s.time_direct_ms, 0),
);
let tailDelegated = $derived(
  tailAttended.reduce((sum, s) => sum + s.time_delegated_ms, 0),
);

let maxDirect = $derived(
  visibleAttended.length > 0
    ? visibleAttended[0].time_direct_ms
    : report.totals.unassigned_direct_ms,
);

let unattendedDelegated = $derived(
  unattendedStreams.reduce((sum, s) => sum + s.time_delegated_ms, 0),
);

// Calculate wall clock time from period
let wallClockMs = $derived(report.totals.total_tracked_ms);
</script>

<div class="flex flex-col gap-3">
  <div class="flex items-center justify-between">
    <h2 class="text-sm font-medium text-[var(--color-text-muted)] uppercase tracking-wider">TIME</h2>
    <div class="flex bg-[var(--color-bg-surface)] rounded-md p-0.5 text-xs">
      <button 
        class="px-2 py-1 rounded-sm transition-colors cursor-pointer {period === 'day' ? 'bg-[var(--color-bg-surface-hover)] text-[var(--color-text-base)] shadow-sm' : 'text-[var(--color-text-muted)] hover:text-[var(--color-text-base)]'}"
        onclick={() => onPeriodChange('day')}
        aria-label="Show today's time"
      >
        Today
      </button>
      <button 
        class="px-2 py-1 rounded-sm transition-colors cursor-pointer {period === 'week' ? 'bg-[var(--color-bg-surface-hover)] text-[var(--color-text-base)] shadow-sm' : 'text-[var(--color-text-muted)] hover:text-[var(--color-text-base)]'}"
        onclick={() => onPeriodChange('week')}
        aria-label="Show this week's time"
      >
        This Week
      </button>
    </div>
  </div>

  <div class="flex flex-col gap-2">
    {#each visibleAttended as stream}
      <div class="flex flex-col gap-1">
        <div class="flex items-start justify-between text-sm">
          <span class="font-medium text-[var(--color-text-base)] line-clamp-2 break-words pr-2" title={stream.name || stream.id}>
            {stream.name || stream.id}
          </span>
          <div class="flex items-center gap-2 shrink-0">
            <span class="font-medium text-[var(--color-text-base)]">{formatDuration(stream.time_direct_ms)}</span>
            {#if stream.time_delegated_ms > 0}
              <span class="text-xs text-[var(--color-text-muted)]">+{formatDuration(stream.time_delegated_ms)}</span>
            {/if}
          </div>
        </div>
        <div class="h-1.5 w-full bg-[var(--color-bg-surface)] rounded-full overflow-hidden">
          <div 
            class="h-full bg-[var(--color-status-blue)] rounded-full" 
            style="width: {maxDirect > 0 ? Math.min(100, (stream.time_direct_ms / maxDirect) * 100) : 0}%"
          ></div>
        </div>
      </div>
    {/each}

    {#if tailAttended.length > 0}
      <div class="text-xs text-[var(--color-text-muted)] italic py-1">
        (+ {tailAttended.length} streams under 15m, {formatDuration(tailDirect)} total{#if tailDelegated > 0}, +{formatDuration(tailDelegated)} delegated{/if})
      </div>
    {/if}

    {#if unattendedStreams.length > 0}
      <div class="text-xs text-[var(--color-text-muted)] italic py-1">
        (+ {unattendedStreams.length} streams with no direct time, {formatDuration(unattendedDelegated)} delegated)
      </div>
    {/if}

    <div class="flex flex-col gap-1 mt-1">
      <div class="flex items-center justify-between text-sm">
        <span class="font-medium text-[var(--color-text-muted)] italic">(unassigned)</span>
        <div class="flex items-center gap-2 shrink-0">
          <span class="font-medium text-[var(--color-text-muted)]">{formatDuration(report.totals.unassigned_direct_ms)}</span>
          {#if report.totals.unassigned_delegated_ms > 0}
            <span class="text-xs text-[var(--color-text-muted)]">+{formatDuration(report.totals.unassigned_delegated_ms)}</span>
          {/if}
        </div>
      </div>
      <div class="h-1.5 w-full bg-[var(--color-bg-surface)] rounded-full overflow-hidden">
        <div 
          class="h-full bg-[var(--color-text-muted)] rounded-full opacity-50" 
          style="width: {maxDirect > 0 ? Math.min(100, (report.totals.unassigned_direct_ms / maxDirect) * 100) : 0}%"
        ></div>
      </div>
    </div>

    {#if junkStream}
      <div class="text-xs text-[var(--color-text-muted)] italic py-1 mt-1">
        (junk: {formatDuration(junkStream.time_direct_ms)} direct, {formatDuration(junkStream.time_delegated_ms)} delegated — not ranked)
      </div>
    {/if}
  </div>

  <div class="mt-2 pt-3 border-t border-[var(--color-border)] flex flex-col gap-1 text-xs text-[var(--color-text-muted)]">
    <div class="flex justify-between">
      <span>Wall clock</span>
      <span>{formatDuration(wallClockMs)}</span>
    </div>
    <div class="flex justify-between">
      <span>Direct time</span>
      <span class="text-[var(--color-text-base)] font-medium">{formatDuration(report.totals.time_direct_ms)}</span>
    </div>
    <div class="flex justify-between">
      <span>Delegated time</span>
      <span>{formatDuration(report.totals.time_delegated_ms)}</span>
    </div>
    <div class="flex justify-between">
      <span>Leverage</span>
      <span>{formatLeverage(report.totals.time_direct_ms, report.totals.time_delegated_ms)}</span>
    </div>
  </div>
</div>
