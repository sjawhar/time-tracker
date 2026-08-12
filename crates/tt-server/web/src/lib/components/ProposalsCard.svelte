<script lang="ts">
import type { Proposal } from '../types';

let {
  proposalsData,
  deciding = null,
  onDecide,
}: {
  proposalsData: { items: Proposal[]; total_pending: number } | null;
  deciding?: string | null;
  onDecide: (id: string, decision: 'accept' | 'reject') => void;
} = $props();

function formatConfidence(conf: number): string {
  return `${Math.round(conf * 100)}%`;
}

function truncateReasoning(text: string): string {
  if (text.length <= 160) return text;
  return `${text.slice(0, 160)}...`;
}
</script>

<div class="flex flex-col p-3 rounded-lg border border-[var(--color-border)] bg-[var(--color-bg-surface)]">
  <div class="flex justify-between items-center mb-2">
    <div class="text-xs font-medium text-[var(--color-text-muted)] uppercase tracking-wider">
      Proposals
    </div>
    {#if proposalsData && proposalsData.total_pending > 0}
      <div class="text-xs font-medium text-[var(--color-text-muted)] bg-[var(--color-bg-base)] px-2 py-0.5 rounded-full">
        {proposalsData.total_pending}
      </div>
    {/if}
  </div>
  
  {#if proposalsData && proposalsData.items.length > 0}
    <div class="flex flex-col gap-2 max-h-[600px] overflow-y-auto pr-1">
      {#each proposalsData.items as proposal (proposal.id)}
        <div class="flex flex-col gap-1.5 p-2 rounded bg-[var(--color-bg-base)] border border-[var(--color-border)]">
          <div class="flex justify-between items-start gap-2">
            <div class="text-sm font-medium text-[var(--color-text-base)] line-clamp-2 break-words" title={proposal.target.kind === 'new' ? `New: ${proposal.target.name}` : proposal.target.name}>
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
            {truncateReasoning(proposal.reasoning)}
          </div>
          
          <!-- The queue is ordered by the attention each decision resolves, so the
               magnitude has to be visible: without it a reviewer sees the right order
               with no way to tell whether item one is worth 104 events or 3, which is
               the whole basis of the ordering. The CLI has always shown this column. -->
          <div class="flex justify-between items-center gap-2 mt-1 pt-1.5 border-t border-[var(--color-border)]">
            <div
              class="text-xs text-[var(--color-text-muted)] shrink-0"
              title={proposal.scope.kind === 'session'
                ? `Resolves ${proposal.scope.count} attention event(s) in this session`
                : `Resolves ${proposal.scope.count} attention event(s)`}
            >
              {proposal.scope.count}
              {proposal.scope.kind === 'session' ? 'in session' : 'events'}
            </div>
            <div class="flex justify-end gap-2">
            <button
              class="text-xs px-2 py-1 rounded bg-[var(--color-bg-surface)] text-[var(--color-text-muted)] hover:text-[var(--color-text-base)] transition-colors cursor-pointer disabled:opacity-50 disabled:cursor-not-allowed"
              title="Reject this proposal; its events stay unassigned"
              disabled={deciding !== null}
              onclick={() => onDecide(proposal.id, 'reject')}
            >
              {deciding === proposal.id ? 'Rejecting...' : 'Reject'}
            </button>
            <button
              class="text-xs px-2 py-1 rounded bg-[var(--color-bg-surface)] text-[var(--color-status-green)] hover:brightness-125 transition-colors cursor-pointer disabled:opacity-50 disabled:cursor-not-allowed"
              title="Accept: assigns these events to the stream as a human verdict"
              disabled={deciding !== null}
              onclick={() => onDecide(proposal.id, 'accept')}
            >
              {deciding === proposal.id ? 'Accepting...' : 'Accept'}
            </button>
            </div>
          </div>
        </div>
      {/each}
      {#if proposalsData.total_pending > proposalsData.items.length}
        <div class="text-xs text-center text-[var(--color-text-muted)] py-1">
          + {proposalsData.total_pending - proposalsData.items.length} more pending
        </div>
      {/if}
    </div>
  {:else}
    <div class="text-sm text-[var(--color-text-muted)] italic py-2 text-center">
      No pending proposals
    </div>
  {/if}
</div>
