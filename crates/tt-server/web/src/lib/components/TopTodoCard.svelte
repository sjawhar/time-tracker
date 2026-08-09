<script lang="ts">
import type { TopTodo } from '../types';

let { todo }: { todo: TopTodo | null } = $props();
</script>

<div class="flex flex-col p-3 rounded-lg border border-[var(--color-border)] bg-[var(--color-bg-surface)]">
  <div class="text-xs font-medium text-[var(--color-text-muted)] mb-1 uppercase tracking-wider">
    Top Priority
  </div>
  
  {#if todo}
    <div class="text-sm text-[var(--color-text-base)] break-words" title={todo.text}>
      {todo.text}
    </div>
    {#if todo.stream_slug}
      <div class="text-xs text-[var(--color-text-muted)] mt-1.5 break-words">
        Stream: {todo.stream_slug}
      </div>
    {/if}
  {:else}
    <div class="text-sm text-[var(--color-text-muted)] italic">
      No top priority set
    </div>
  {/if}
</div>
