<script lang="ts">
import type { ClassifierHealth } from '../types';

let { health }: { health: ClassifierHealth } = $props();

let isFailing = $derived(health.consecutive_failures > 0);

function getShortError(err: string | null): string {
  if (!err) return 'Unknown error';
  try {
    const parsed = JSON.parse(err);
    if (parsed.error?.message) return parsed.error.message;
    if (parsed.message) return parsed.message;
    if (parsed.type) return `${parsed.type} error`;
  } catch (e) {
    // Not JSON
  }
  // If it's a long string, take the first line or first 50 chars
  const firstLine = err.split('\n')[0];
  return firstLine.length > 50 ? `${firstLine.substring(0, 50)}...` : firstLine;
}
</script>

<div 
  class="flex flex-col p-3 rounded-lg border border-[var(--color-border)] bg-[var(--color-bg-surface)]"
  style={isFailing ? 'border-color: var(--color-status-red); background-color: var(--color-status-red-bg);' : ''}
>
  <div class="flex justify-between items-center w-full">
    <div class="text-xs font-medium uppercase tracking-wider" style={isFailing ? 'color: var(--color-status-red)' : 'color: var(--color-text-muted)'}>
      Classifier Health
    </div>
  </div>
  
  {#if isFailing}
    <div class="text-sm font-medium text-[var(--color-status-red)] mt-1.5 break-words" title={health.last_error || 'Failing'}>
      Failing ({health.consecutive_failures}x) — {getShortError(health.last_error)}
    </div>
  {:else}
    <div class="text-sm text-[var(--color-text-base)] mt-1">
      Healthy
    </div>
  {/if}
</div>
