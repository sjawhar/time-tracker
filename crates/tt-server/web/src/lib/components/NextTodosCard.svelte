<script lang="ts">
import type { Todo } from '../types';
import StreamPicker from './StreamPicker.svelte';

let {
  todos,
  onSetStream,
}: {
  todos: Todo[] | null;
  onSetStream?: (todoId: string, streamId: string | null) => Promise<void>;
} = $props();

let expanded = $state(false);
let pickerOpenFor = $state<string | null>(null);
let streamError = $state<{ id: string; message: string } | null>(null);

async function handleSetStream(todoId: string, streamId: string | null) {
  if (!onSetStream) return;
  try {
    streamError = null;
    await onSetStream(todoId, streamId);
    pickerOpenFor = null;
  } catch (e) {
    streamError = {
      id: todoId,
      message: e instanceof Error ? e.message : String(e),
    };
  }
}

let displayTodos = $derived(() => {
  if (!todos) return [];
  return todos;
});
</script>

<div class="flex flex-col p-3 rounded-lg border border-[var(--color-border)] bg-[var(--color-bg-surface)]">
  <div class="flex justify-between items-center mb-2">
    <div class="text-xs font-medium text-[var(--color-text-muted)] uppercase tracking-wider">
      Next Todos
    </div>
  </div>
  
  {#if todos && todos.length > 0}
    <div class="flex flex-col gap-2 max-h-[50vh] overflow-y-auto pr-1">
      {#each displayTodos() as todo (todo.id)}
        <div class="flex flex-col gap-1 p-2 rounded bg-[var(--color-bg-base)] border border-[var(--color-border)]">
          <div class="text-sm text-[var(--color-text-base)] break-words" title={todo.text}>
            {todo.text}
          </div>
          <div class="flex items-center justify-between mt-1">
            <div class="relative flex items-center gap-1 max-w-[60%]">
              <button 
                class="text-xs text-[var(--color-text-muted)] hover:text-[var(--color-text-base)] transition-colors line-clamp-2 break-words text-left cursor-pointer"
                title={todo.stream_slug || 'Unassigned'}
                onclick={() => pickerOpenFor = pickerOpenFor === todo.id ? null : todo.id}
              >
                {todo.stream_slug || 'Unassigned'}
              </button>
              {#if todo.stream_slug}
                <button 
                  class="text-[var(--color-text-muted)] hover:text-[var(--color-status-red)] transition-colors cursor-pointer px-1"
                  title="Clear stream"
                  onclick={() => handleSetStream(todo.id, null)}
                >
                  ×
                </button>
              {/if}
              {#if pickerOpenFor === todo.id}
                <StreamPicker 
                  onSelect={(streamId) => handleSetStream(todo.id, streamId)}
                  onClose={() => pickerOpenFor = null}
                />
              {/if}
            </div>
            {#if streamError?.id === todo.id}
              <div class="text-xs text-[var(--color-status-red)] mt-1">{streamError.message}</div>
            {/if}
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
