<script lang="ts">
import type { WipStatus } from '../types';

let { wip }: { wip: WipStatus } = $props();

let isOverLimit = $derived(wip.in_flight.length > wip.limit);
</script>

<div 
  class="flex flex-col p-3 rounded-lg border border-[var(--color-border)] bg-[var(--color-bg-surface)]"
  style={isOverLimit ? 'border-color: var(--color-status-amber); background-color: var(--color-status-amber-bg);' : ''}
>
  <div class="flex justify-between items-center mb-1.5 gap-2 w-full">
    <div class="text-xs font-medium uppercase tracking-wider min-w-0" style={isOverLimit ? 'color: var(--color-status-amber)' : 'color: var(--color-text-muted)'}>
      WIP ({wip.in_flight.length}/{wip.limit})
    </div>
  </div>
  
  {#if wip.in_flight.length > 0}
    <ul class="flex flex-col gap-1">
      {#each wip.in_flight as stream}
        <li class="text-sm text-[var(--color-text-base)] break-words" title={stream.name}>
          • {stream.name}
        </li>
      {/each}
    </ul>
  {:else}
    <div class="text-sm text-[var(--color-text-muted)] italic">
      No streams in flight
    </div>
  {/if}

  {#if isOverLimit && wip.wind_down_candidate}
    <div class="mt-3 pt-2 border-t border-[var(--color-border)]">
      <div class="text-xs text-[var(--color-status-amber)] font-medium mb-1">
        Wind down candidate:
      </div>
      <div class="text-sm text-[var(--color-text-base)] break-words" title={wip.wind_down_candidate}>
        {wip.wind_down_candidate}
      </div>
    </div>
  {/if}
</div>
