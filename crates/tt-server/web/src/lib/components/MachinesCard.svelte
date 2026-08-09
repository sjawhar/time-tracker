<script lang="ts">
import type { MachineFreshness } from '../types';

let { machines }: { machines: MachineFreshness[] } = $props();

function formatTimeAgo(dateStr: string | null): string {
  if (!dateStr) return 'Never';

  const date = new Date(dateStr);
  const now = new Date();
  const diffMs = now.getTime() - date.getTime();
  const diffMins = Math.floor(diffMs / 60000);

  if (diffMins < 1) return 'Just now';
  if (diffMins < 60) return `${diffMins}m ago`;

  const hours = Math.floor(diffMins / 60);
  if (hours < 24) return `${hours}h ago`;

  const days = Math.floor(hours / 24);
  return `${days}d ago`;
}
</script>

<div class="flex flex-col p-3 rounded-lg border border-[var(--color-border)] bg-[var(--color-bg-surface)]">
  <div class="text-xs font-medium text-[var(--color-text-muted)] uppercase tracking-wider mb-1.5">
    Machines
  </div>
  
  {#if machines.length > 0}
    <ul class="flex flex-col gap-2 w-full">
      {#each machines as machine}
        <li class="flex justify-between items-center gap-2 w-full">
          <span class="text-sm text-[var(--color-text-base)] break-words min-w-0" title={machine.label}>
            {machine.label}
          </span>
          <span class="sr-only"> </span>
          <span class="text-xs text-[var(--color-text-muted)] whitespace-nowrap shrink-0">
            {formatTimeAgo(machine.last_sync_at)}
          </span>
        </li>
      {/each}
    </ul>
  {:else}
    <div class="text-sm text-[var(--color-text-muted)] italic">
      No machines configured
    </div>
  {/if}
</div>
