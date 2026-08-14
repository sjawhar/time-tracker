<script lang="ts">
import { onDestroy, onMount } from 'svelte';
import { mergeStreams, updateStream } from '../api';
import type { ReportStream, Stream } from '../types';

let {
  stream,
  allStreams,
  onClose,
  onUpdate,
}: {
  stream: ReportStream;
  allStreams: ReportStream[];
  onClose: () => void;
  onUpdate: () => void;
} = $props();

let name = $state(stream.name || '');
let description = $state(''); // We don't have description in ReportStream, might need to fetch it or just allow setting it
let color = $state('');
let tags = $state(stream.tags.join(', '));
let mergeTarget = $state('');
let isSaving = $state(false);
let error = $state<string | null>(null);

// We need to fetch the full stream details to get description and color
let fullStream = $state<Stream | null>(null);
let isLoading = $state(true);

onMount(async () => {
  try {
    // We don't have a GET /api/streams/{id} endpoint listed in the prompt, but we can just use the data we have and allow updating.
    // Wait, the prompt says: "PATCH /api/streams/{id} body {name?, description?, color?, add_tags?: string[], remove_tags?: string[]} → 200 {stream_id, name, description, color, tags}"
    // I'll just use the initial values from ReportStream and let the user overwrite them.
    isLoading = false;
  } catch (e) {
    error = e instanceof Error ? e.message : String(e);
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

async function saveField(field: 'name' | 'description' | 'color' | 'tags') {
  if (isSaving) return;
  isSaving = true;
  error = null;
  try {
    const payload: Record<string, unknown> = {};
    if (field === 'name') payload.name = name;
    if (field === 'description') payload.description = description;
    if (field === 'color') payload.color = color;
    if (field === 'tags') {
      // Simple tag parsing: comma separated
      const newTags = tags
        .split(',')
        .map((t) => t.trim())
        .filter((t) => t);
      // For simplicity, we can just send add_tags and remove_tags based on diff, or if the API supports it, just update.
      // The prompt says: add_tags?: string[], remove_tags?: string[]
      const oldTags = stream.tags;
      payload.add_tags = newTags.filter((t) => !oldTags.includes(t));
      payload.remove_tags = oldTags.filter((t) => !newTags.includes(t));
    }

    await updateStream(stream.id, payload);
    onUpdate();
  } catch (e) {
    error = e instanceof Error ? e.message : String(e);
  } finally {
    isSaving = false;
  }
}

async function handleMerge() {
  if (!mergeTarget || isSaving) return;
  isSaving = true;
  error = null;
  try {
    await mergeStreams(mergeTarget, { sources: [stream.id] });
    onUpdate();
    onClose();
  } catch (e) {
    error = e instanceof Error ? e.message : String(e);
  } finally {
    isSaving = false;
  }
}
</script>

<div class="fixed inset-0 z-50 flex justify-end bg-black/50">
  <div class="w-96 bg-[var(--color-bg-base)] h-full border-l border-[var(--color-border)] p-4 flex flex-col gap-4 overflow-y-auto shadow-xl">
    <div class="flex justify-between items-center">
      <h2 class="text-lg font-medium text-[var(--color-text-base)]">Stream Details</h2>
      <button class="text-[var(--color-text-muted)] hover:text-[var(--color-text-base)] cursor-pointer" onclick={onClose}>
        ✕
      </button>
    </div>

    {#if error}
      <div class="text-xs text-[var(--color-status-red)] bg-[var(--color-status-red)]/10 p-2 rounded">
        {error}
      </div>
    {/if}

    <div class="flex flex-col gap-1">
      <label class="text-xs text-[var(--color-text-muted)]">Name</label>
      <input 
        type="text" 
        bind:value={name} 
        onblur={() => saveField('name')}
        onkeydown={(e) => e.key === 'Enter' && saveField('name')}
        class="bg-[var(--color-bg-surface)] border border-[var(--color-border)] rounded p-1.5 text-sm text-[var(--color-text-base)] focus:outline-none focus:border-[var(--color-status-blue)]"
      />
    </div>

    <div class="flex flex-col gap-1">
      <label class="text-xs text-[var(--color-text-muted)]">Description</label>
      <textarea 
        bind:value={description} 
        onblur={() => saveField('description')}
        class="bg-[var(--color-bg-surface)] border border-[var(--color-border)] rounded p-1.5 text-sm text-[var(--color-text-base)] focus:outline-none focus:border-[var(--color-status-blue)] min-h-[80px]"
      ></textarea>
    </div>

    <div class="flex flex-col gap-1">
      <label class="text-xs text-[var(--color-text-muted)]">Color</label>
      <input 
        type="text" 
        bind:value={color} 
        onblur={() => saveField('color')}
        onkeydown={(e) => e.key === 'Enter' && saveField('color')}
        class="bg-[var(--color-bg-surface)] border border-[var(--color-border)] rounded p-1.5 text-sm text-[var(--color-text-base)] focus:outline-none focus:border-[var(--color-status-blue)]"
        placeholder="#RRGGBB or color name"
      />
    </div>

    <div class="flex flex-col gap-1">
      <label class="text-xs text-[var(--color-text-muted)]">Tags (comma separated)</label>
      <input 
        type="text" 
        bind:value={tags} 
        onblur={() => saveField('tags')}
        onkeydown={(e) => e.key === 'Enter' && saveField('tags')}
        class="bg-[var(--color-bg-surface)] border border-[var(--color-border)] rounded p-1.5 text-sm text-[var(--color-text-base)] focus:outline-none focus:border-[var(--color-status-blue)]"
      />
    </div>

    <div class="mt-4 pt-4 border-t border-[var(--color-border)] flex flex-col gap-2">
      <h3 class="text-sm font-medium text-[var(--color-text-base)]">Merge into...</h3>
      <p class="text-xs text-[var(--color-text-muted)]">Move all events from this stream into another stream, then retire this one.</p>
      
      <select 
        bind:value={mergeTarget}
        class="bg-[var(--color-bg-surface)] border border-[var(--color-border)] rounded p-1.5 text-sm text-[var(--color-text-base)] focus:outline-none focus:border-[var(--color-status-blue)]"
      >
        <option value="">Select target stream...</option>
        {#each allStreams.filter(s => s.id !== stream.id && s.id !== 'junk') as target}
          <option value={target.id}>{target.name || target.id}</option>
        {/each}
      </select>
      
      <button 
        class="mt-2 px-3 py-1.5 bg-[var(--color-status-red)]/20 text-[var(--color-status-red)] rounded text-sm font-medium hover:bg-[var(--color-status-red)]/30 transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
        disabled={!mergeTarget || isSaving}
        onclick={handleMerge}
      >
        {isSaving ? 'Merging...' : 'Confirm Merge'}
      </button>
    </div>
  </div>
</div>
