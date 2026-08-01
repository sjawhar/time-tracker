<script lang="ts">
import type { Todo } from '../types';

let { todos }: { todos: Todo[] | null } = $props();

let expanded = $state(false);

let displayTodos = $derived(() => {
  if (!todos) return [];
  return expanded ? todos : todos.slice(0, 3);
});
</script>

<div class="flex flex-col p-3 rounded-lg border border-[var(--color-border)] bg-[var(--color-bg-surface)]">
  <div class="flex justify-between items-center mb-2">
    <div class="text-xs font-medium text-[var(--color-text-muted)] uppercase tracking-wider">
      Next Todos
    </div>
    {#if todos && todos.length > 3}
      <button 
        class="text-xs text-[var(--color-text-muted)] hover:text-[var(--color-text-base)] transition-colors"
        onclick={() => expanded = !expanded}
      >
        {expanded ? 'Show less' : `Show all (${todos.length})`}
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
            <div class="text-xs text-[var(--color-text-muted)] truncate max-w-[60%]" title={todo.stream_slug || 'Unassigned'}>
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
    </div>
  {:else}
    <div class="text-sm text-[var(--color-text-muted)] italic py-2 text-center">
      No pending todos
    </div>
  {/if}
</div>
