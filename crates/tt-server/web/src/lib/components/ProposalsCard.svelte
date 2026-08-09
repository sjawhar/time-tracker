<script lang="ts">
import type { Proposal } from '../types';

let { proposals }: { proposals: Proposal[] | null } = $props();

function formatConfidence(conf: number): string {
  return `${Math.round(conf * 100)}%`;
}
</script>

<div class="flex flex-col p-3 rounded-lg border border-[var(--color-border)] bg-[var(--color-bg-surface)]">
  <div class="flex justify-between items-center mb-2">
    <div class="text-xs font-medium text-[var(--color-text-muted)] uppercase tracking-wider">
      Proposals
    </div>
    {#if proposals && proposals.length > 0}
      <div class="text-xs font-medium text-[var(--color-text-muted)] bg-[var(--color-bg-base)] px-2 py-0.5 rounded-full">
        {proposals.length}
      </div>
    {/if}
  </div>
  
  {#if proposals && proposals.length > 0}
    <div class="flex flex-col gap-2">
      {#each proposals as proposal (proposal.id)}
        <div class="flex flex-col gap-1.5 p-2 rounded bg-[var(--color-bg-base)] border border-[var(--color-border)]">
          <div class="flex justify-between items-start gap-2">
            <div class="text-sm font-medium text-[var(--color-text-base)] truncate" title={proposal.target.kind === 'new' ? `New: ${proposal.target.name}` : proposal.target.name}>
              {#if proposal.target.kind === 'new'}
                <span class="text-[var(--color-status-blue)]">New:</span> {proposal.target.name}
              {:else}
                {proposal.target.name}
              {/if}
            </div>
            <div class="text-xs font-medium {proposal.confidence >= 0.8 ? 'text-[var(--color-status-green)]' : 'text-[var(--color-status-amber)]'} shrink-0">
              {formatConfidence(proposal.confidence)}
            </div>
          </div>
          
          <div class="text-xs text-[var(--color-text-muted)] line-clamp-2" title={proposal.reasoning}>
            {proposal.reasoning}
          </div>
          
          <div class="flex justify-end gap-2 mt-1 pt-1.5 border-t border-[var(--color-border)]">
            <button class="text-xs px-2 py-1 rounded bg-[var(--color-bg-surface)] text-[var(--color-text-muted)] opacity-50 cursor-not-allowed" title="Not yet wired" disabled>
              Reject
            </button>
            <button class="text-xs px-2 py-1 rounded bg-[var(--color-bg-surface)] text-[var(--color-text-muted)] opacity-50 cursor-not-allowed" title="Not yet wired" disabled>
              Accept
            </button>
          </div>
        </div>
      {/each}
    </div>
  {:else}
    <div class="text-sm text-[var(--color-text-muted)] italic py-2 text-center">
      No pending proposals
    </div>
  {/if}
</div>
