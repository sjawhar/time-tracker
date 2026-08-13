<script lang="ts">
import { onDestroy, onMount } from 'svelte';
import { fetchTodos } from '../api';
import type { Todo } from '../types';

let {
  onSelect,
  onClose,
}: {
  onSelect: (todoId: string) => void;
  onClose: () => void;
} = $props();

let todos = $state<Todo[]>([]);
let isLoading = $state(true);
let error = $state<string | null>(null);

onMount(async () => {
  try {
    const data = await fetchTodos();
    todos = data.todos;
  } catch (e) {
    error = e instanceof Error ? e.message : String(e);
  } finally {
    isLoading = false;
  }
});

function handleKeydown(e: KeyboardEvent) {
  if (e.key === 'Escape') {
    onClose();
  }
}

onMount(() => {
  window.addEventListener('keydown', handleKeydown);
});

onDestroy(() => {
  window.removeEventListener('keydown', handleKeydown);
});
</script>

<div class="absolute z-50 mt-1 w-64 bg-[var(--color-bg-base)] border border-[var(--color-border)] rounded shadow-lg max-h-64 overflow-y-auto right-0">
  {#if isLoading}
    <div class="p-2 text-xs text-[var(--color-text-muted)]">Loading todos...</div>
  {:else if error}
    <div class="p-2 text-xs text-[var(--color-status-red)]">{error}</div>
  {:else if todos.length === 0}
    <div class="p-2 text-xs text-[var(--color-text-muted)]">No open todos</div>
  {:else}
    <div class="flex flex-col">
      {#each todos as todo}
        <button 
          class="text-left p-2 text-xs text-[var(--color-text-base)] hover:bg-[var(--color-bg-surface)] border-b border-[var(--color-border)] last:border-0 cursor-pointer"
          onclick={() => onSelect(todo.id)}
        >
          <div class="line-clamp-2">{todo.text}</div>
          {#if todo.stream_slug}
            <div class="text-[var(--color-text-muted)] mt-0.5">{todo.stream_slug}</div>
          {/if}
        </button>
      {/each}
    </div>
  {/if}
</div>
