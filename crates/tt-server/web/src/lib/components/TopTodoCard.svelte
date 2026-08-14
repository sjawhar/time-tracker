<script lang="ts">
import type { TopTodo } from '../types';
import StreamPicker from './StreamPicker.svelte';

let {
  todo,
  onSetStream,
}: {
  todo: TopTodo | null;
  onSetStream?: (todoId: string, streamId: string | null) => Promise<void>;
} = $props();

let pickerOpen = $state(false);
let streamError = $state<string | null>(null);

async function handleSetStream(streamId: string | null) {
  if (!todo || !onSetStream) return;
  try {
    streamError = null;
    await onSetStream(todo.id, streamId);
    pickerOpen = false;
  } catch (e) {
    streamError = e instanceof Error ? e.message : String(e);
  }
}
</script>

<div class="flex flex-col p-3 rounded-lg border border-[var(--color-border)] bg-[var(--color-bg-surface)]">
  <div class="text-xs font-medium text-[var(--color-text-muted)] mb-1 uppercase tracking-wider">
    Top Priority
  </div>
  
  {#if todo}
    <div class="text-sm text-[var(--color-text-base)] break-words" title={todo.text}>
      {todo.text}
    </div>
    <div class="relative flex items-center gap-1 mt-1.5">
      <span class="text-xs text-[var(--color-text-muted)]">Stream:</span>
      <button 
        class="text-xs text-[var(--color-text-muted)] hover:text-[var(--color-text-base)] transition-colors line-clamp-1 break-words text-left cursor-pointer"
        title={todo.stream_slug || 'Unassigned'}
        onclick={() => pickerOpen = !pickerOpen}
      >
        {todo.stream_slug || 'Unassigned'}
      </button>
      {#if todo.stream_slug}
        <button 
          class="text-[var(--color-text-muted)] hover:text-[var(--color-status-red)] transition-colors cursor-pointer px-1"
          title="Clear stream"
          onclick={() => handleSetStream(null)}
        >
          ×
        </button>
      {/if}
      {#if pickerOpen}
        <StreamPicker 
          onSelect={(streamId) => handleSetStream(streamId)}
          onClose={() => pickerOpen = false}
        />
      {/if}
    </div>
    {#if streamError}
      <div class="text-xs text-[var(--color-status-red)] mt-1">{streamError}</div>
    {/if}
  {:else}
    <div class="text-sm text-[var(--color-text-muted)] italic">
      No top priority set
    </div>
  {/if}
</div>
