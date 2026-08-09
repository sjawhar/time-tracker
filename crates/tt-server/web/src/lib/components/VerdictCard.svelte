<script lang="ts">
import type { Verdict } from '../types';

let { verdict }: { verdict: Verdict } = $props();

let isAligned = $derived(verdict.aligned === true);
let isDrifting = $derived(verdict.aligned === false);
let isUnknown = $derived(verdict.aligned === null);

let statusColor = $derived(
  isAligned
    ? 'var(--color-status-green)'
    : isDrifting
      ? 'var(--color-status-red)'
      : 'var(--color-text-muted)',
);

let statusBg = $derived(
  isAligned
    ? 'var(--color-status-green-bg)'
    : isDrifting
      ? 'var(--color-status-red-bg)'
      : 'transparent',
);

function formatDuration(since: string): string {
  const start = new Date(since).getTime();
  const now = Date.now();
  const diffMins = Math.floor((now - start) / 60000);

  if (diffMins < 60) return `${diffMins}m`;
  const hours = Math.floor(diffMins / 60);
  const mins = diffMins % 60;
  return `${hours}h ${mins}m`;
}
</script>

<div 
  class="flex flex-col p-5 rounded-xl border-2 shadow-lg @container"
  style="background-color: {statusBg}; border-color: {isUnknown ? 'var(--color-border)' : statusColor}"
>
  <div class="text-[min(3.75rem,18.5cqi)] font-black tracking-tighter mb-4" style="color: {statusColor}">
    {#if isAligned}
      ALIGNED
    {:else if isDrifting}
      DRIFTING
    {:else}
      UNKNOWN
    {/if}
  </div>
  
  {#if verdict.current_stream}
    <div class="text-base font-medium text-[var(--color-text-base)] break-words leading-snug" title={verdict.current_stream.name}>
      {verdict.current_stream.name}
    </div>
    <div class="text-sm font-semibold mt-2 opacity-80" style="color: {statusColor}">
      for {formatDuration(verdict.current_stream.since)}
    </div>
  {:else}
    <div class="text-base font-medium text-[var(--color-text-muted)]">No active stream</div>
  {/if}
</div>
