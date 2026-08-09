<script lang="ts">
import type { Session } from '../types';

let { sessions }: { sessions: Session[] | null } = $props();

// 10 minutes in milliseconds
const QUIET_THRESHOLD_MS = 10 * 60 * 1000;

function isQuiet(lastActivityStr: string): boolean {
  const lastActivity = new Date(lastActivityStr).getTime();
  const now = Date.now();
  return now - lastActivity > QUIET_THRESHOLD_MS;
}

function formatDuration(ms: number): string {
  const minutes = Math.floor(ms / 60000);
  if (minutes < 60) return `${minutes}m`;
  const hours = Math.floor(minutes / 60);
  const remainingMins = minutes % 60;
  return `${hours}h ${remainingMins}m`;
}
</script>

<div class="flex flex-col p-3 rounded-lg border border-[var(--color-border)] bg-[var(--color-bg-surface)]">
  <div class="flex justify-between items-center mb-2">
    <div class="text-xs font-medium text-[var(--color-text-muted)] uppercase tracking-wider">
      Agent Sessions
    </div>
    {#if sessions && sessions.length > 0}
      <div class="text-xs font-medium text-[var(--color-text-muted)] bg-[var(--color-bg-base)] px-2 py-0.5 rounded-full">
        {sessions.length}
      </div>
    {/if}
  </div>
  
  {#if sessions && sessions.length > 0}
    <div class="flex flex-col gap-2">
      {#each sessions as session (session.session_id)}
        {@const quiet = isQuiet(session.last_activity)}
        <div class="flex flex-col gap-1.5 p-2 rounded bg-[var(--color-bg-base)] border {quiet ? 'border-[var(--color-status-amber)]/50' : 'border-[var(--color-border)]'}">
          <div class="flex justify-between items-start gap-2">
            <div class="flex items-center gap-1.5 min-w-0">
              <div class="w-2 h-2 rounded-full shrink-0 {quiet ? 'bg-[var(--color-status-amber)]' : 'bg-[var(--color-status-green)] animate-pulse'}" title={quiet ? 'Quiet (>10m no activity)' : 'Active'}></div>
              <div class="text-sm font-medium text-[var(--color-text-base)] truncate" title={session.harness}>
                {session.harness}
              </div>
              {#if session.machine_label}
                <div class="text-xs text-[var(--color-text-muted)] bg-[var(--color-bg-surface)] px-1.5 py-0.5 rounded truncate max-w-[80px]" title={session.machine_label}>
                  {session.machine_label}
                </div>
              {/if}
            </div>
            <div class="text-xs text-[var(--color-text-muted)] shrink-0 tabular-nums">
              {formatDuration(session.duration_ms)}
            </div>
          </div>
          
          <div class="flex items-center gap-1.5">
            {#if session.stream}
              <div class="text-xs text-[var(--color-text-base)] truncate" title={session.stream.name}>
                {session.stream.name}
              </div>
            {:else}
              <div class="text-xs text-[var(--color-text-muted)] italic">
                Unclassified
              </div>
            {/if}
          </div>
          
          {#if session.linked_todo_text}
            <div class="text-xs text-[var(--color-text-muted)] truncate border-t border-[var(--color-border)] pt-1.5 mt-0.5" title={session.linked_todo_text}>
              <span class="opacity-70">↳</span> {session.linked_todo_text}
            </div>
          {:else}
            <div class="text-xs text-[var(--color-text-muted)] italic border-t border-dashed border-[var(--color-border)] pt-1.5 mt-0.5 flex justify-between items-center">
              <span>Unlinked</span>
              <button class="text-[var(--color-text-muted)] hover:text-[var(--color-text-base)] transition-colors" title="Linking not yet wired" disabled>
                Link...
              </button>
            </div>
          {/if}
        </div>
      {/each}
    </div>
  {:else}
    <div class="text-sm text-[var(--color-text-muted)] italic py-2 text-center">
      No active sessions
    </div>
  {/if}
</div>
