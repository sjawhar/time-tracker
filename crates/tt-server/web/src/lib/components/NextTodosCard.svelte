<script lang="ts">
import type { Todo } from '../types';

let { todos }: { todos: Todo[] | null } = $props();

let expanded = $state(false);

let displayTodos = $derived(() => {
  if (!todos) return [];
  return expanded ? todos.slice(0, 12) : todos.slice(0, 3);
});

let remainingTodos = $derived(() => {
  if (!todos) return 0;
  return todos.length - displayTodos().length;
});
</script>

<div class="flex flex-col p-3 rounded-lg border border-[var(--color-border)] bg-[var(--color-bg-surface)]">
  <div class="flex justify-between items-center mb-2">
    <div class="text-xs font-medium text-[var(--color-text-muted)] uppercase tracking-wider">
      Next Todos
    </div>
    {#if todos && todos.length > 3}
      <!-- min-h-6/px-1.5 keep this a WCAG 2.2 SC 2.5.8 target (24x24 CSS px minimum).
           text-xs alone gave it a 16px box, which is the one control on the page that
           failed that floor -- and it is the only route to the rest of the queue. -->
      <button
        class="text-xs text-[var(--color-text-muted)] hover:text-[var(--color-text-base)] transition-colors cursor-pointer min-h-6 px-1.5 py-1 -mr-1.5 inline-flex items-center"
        onclick={() => expanded = !expanded}
      >
        {expanded ? 'Show less' : `Show more (${todos.length})`}
      </button>
    {/if}
  </div>
  
  {#if todos && todos.length > 0}
    <div class="flex flex-col gap-2">
      {#each displayTodos() as todo (todo.id)}
        <div class="flex flex-col gap-1 p-2 rounded bg-[var(--color-bg-base)] border border-[var(--color-border)]">
          <div class="text-sm text-[var(--color-text-base)] break-words" title={todo.text}>
            {todo.text}
          </div>
          <div class="flex items-center justify-between mt-1">
            <div class="text-xs text-[var(--color-text-muted)] line-clamp-2 break-words max-w-[60%]" title={todo.stream_slug || 'Unassigned'}>
              {todo.stream_slug || 'Unassigned'}
            </div>
            {#if todo.linked_agent_count > 0}
              <div class="text-xs font-medium text-[var(--color-status-blue)] bg-[var(--color-status-blue)]/10 px-1.5 py-0.5 rounded flex items-center gap-1 shrink-0">
                <span class="w-1.5 h-1.5 rounded-full bg-[var(--color-status-blue)] animate-pulse"></span>
                {todo.linked_agent_count} agent{todo.linked_agent_count === 1 ? '' : 's'} running
              </div>
            {/if}
          </div>
        </div>
      {/each}
      {#if expanded && remainingTodos() > 0}
        <div class="text-xs text-center text-[var(--color-text-muted)] py-1">
          + {remainingTodos()} more pending
        </div>
      {/if}
    </div>
  {:else}
    <div class="text-sm text-[var(--color-text-muted)] italic py-2 text-center">
      No pending todos
    </div>
  {/if}
</div>
