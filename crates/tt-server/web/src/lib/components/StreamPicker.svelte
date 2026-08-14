<script module lang="ts">
import { listStreams } from '../api';

type StreamItem = {
  id: string;
  name: string | null;
  slug: string | null;
  last_active: string | null;
};

let cachedStreams: StreamItem[] | null = null;
let fetchPromise: Promise<{ streams: StreamItem[] }> | null = null;

export async function getStreams(): Promise<StreamItem[]> {
  if (cachedStreams) return cachedStreams;
  if (!fetchPromise) {
    fetchPromise = listStreams();
  }
  try {
    const data = await fetchPromise;
    cachedStreams = data.streams;
    return cachedStreams;
  } catch (e) {
    fetchPromise = null;
    throw e;
  }
}
</script>

<script lang="ts">
import { onDestroy, onMount } from 'svelte';

let {
  onSelect,
  onClose,
}: {
  onSelect: (streamId: string) => void;
  onClose: () => void;
} = $props();

let streams = $state<StreamItem[]>([]);
let isLoading = $state(true);
let error = $state<string | null>(null);
let filterText = $state('');
let inputRef = $state<HTMLInputElement | null>(null);

onMount(async () => {
  try {
    streams = await getStreams();
  } catch (e) {
    error = e instanceof Error ? e.message : String(e);
  } finally {
    isLoading = false;
  }
  if (inputRef) {
    inputRef.focus();
  }
});

let filteredStreams = $derived(() => {
  if (!filterText.trim()) return streams.slice(0, 20);
  const lower = filterText.toLowerCase();
  return streams
    .filter(s => 
      (s.name && s.name.toLowerCase().includes(lower)) || 
      (s.slug && s.slug.toLowerCase().includes(lower))
    )
    .slice(0, 20);
});

function handleKeydown(e: KeyboardEvent) {
  if (e.key === 'Escape') {
    onClose();
  } else if (e.key === 'Enter') {
    const results = filteredStreams();
    if (results.length > 0) {
      onSelect(results[0].id);
    }
  }
}

onMount(() => {
  window.addEventListener('keydown', handleKeydown);
});

onDestroy(() => {
  window.removeEventListener('keydown', handleKeydown);
});
</script>

<div class="absolute z-50 mt-1 w-64 bg-[var(--color-bg-base)] border border-[var(--color-border)] rounded shadow-lg max-h-80 flex flex-col right-0">
  <div class="p-2 border-b border-[var(--color-border)]">
    <input
      bind:this={inputRef}
      bind:value={filterText}
      type="text"
      placeholder="Filter streams..."
      class="w-full bg-[var(--color-bg-surface)] border border-[var(--color-border)] rounded px-2 py-1 text-xs text-[var(--color-text-base)] focus:outline-none focus:border-[var(--color-text-muted)]"
    />
  </div>
  <div class="overflow-y-auto flex-1">
    {#if isLoading}
      <div class="p-2 text-xs text-[var(--color-text-muted)]">Loading streams...</div>
    {:else if error}
      <div class="p-2 text-xs text-[var(--color-status-red)]">{error}</div>
    {:else if filteredStreams().length === 0}
      <div class="p-2 text-xs text-[var(--color-text-muted)]">No matching streams</div>
    {:else}
      <div class="flex flex-col">
        {#each filteredStreams() as stream}
          <button 
            class="text-left p-2 text-xs text-[var(--color-text-base)] hover:bg-[var(--color-bg-surface)] border-b border-[var(--color-border)] last:border-0 cursor-pointer"
            onclick={() => onSelect(stream.id)}
          >
            <div class="line-clamp-1 font-medium">{stream.slug || 'Unslugged'}</div>
            {#if stream.name}
              <div class="text-[var(--color-text-muted)] mt-0.5 line-clamp-1">{stream.name}</div>
            {/if}
          </button>
        {/each}
      </div>
    {/if}
  </div>
</div>
