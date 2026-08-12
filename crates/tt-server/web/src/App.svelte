<script lang="ts">
import { onMount } from 'svelte';
import AgentSessionsCard from './lib/components/AgentSessionsCard.svelte';
import ClassifierHealthCard from './lib/components/ClassifierHealthCard.svelte';
import MachinesCard from './lib/components/MachinesCard.svelte';
import NextTodosCard from './lib/components/NextTodosCard.svelte';
import ProposalsCard from './lib/components/ProposalsCard.svelte';
import TimeCard from './lib/components/TimeCard.svelte';
import Timeline from './lib/components/Timeline.svelte';
import TopTodoCard from './lib/components/TopTodoCard.svelte';
import VerdictCard from './lib/components/VerdictCard.svelte';
import WipCard from './lib/components/WipCard.svelte';
import { createStatusStore } from './lib/store.svelte';

const store = createStatusStore();

onMount(() => {
  return store.init();
});
</script>

<main class="flex h-screen w-full overflow-hidden">
  <!-- Cockpit Rail (Left Column) -->
  <aside class="w-full sm:w-[360px] h-full border-r border-[var(--color-border)] bg-[var(--color-bg-base)] flex flex-col overflow-y-auto overflow-x-hidden shrink-0">
    {#if store.loading && !store.verdict}
      <div class="p-4 text-[var(--color-text-muted)]">Loading...</div>
    {:else if store.error}
      <div class="p-4 text-[var(--color-status-red)]">Error: {store.error}</div>
    {:else if store.verdict}
      <div class="flex flex-col gap-5 p-5 min-h-full">
        <VerdictCard verdict={store.verdict} />
        {#if store.report}
          <TimeCard report={store.report} period={store.reportPeriod} onPeriodChange={(p) => store.setReportPeriod(p)} />
        {/if}
        <TopTodoCard todo={store.verdict.top_todo} />
        <WipCard wip={store.verdict.wip} />
        
        <div class="flex-1 flex flex-col gap-4">
          <NextTodosCard todos={store.todos} />
          <AgentSessionsCard sessions={store.sessions} />
          <ProposalsCard proposalsData={store.proposals} deciding={store.decidingProposal}
            onDecide={(id, decision) => store.decideProposal(id, decision)} />
        </div>
        
        <div class="mt-auto pt-5 flex flex-col gap-5">
          <ClassifierHealthCard health={store.verdict.classifier} />
          <MachinesCard machines={store.verdict.machines} />
        </div>
      </div>
    {/if}
  </aside>

  <section class="hidden sm:flex flex-1 h-full bg-[var(--color-bg-base)] p-4">
    {#if store.timeline}
      <Timeline data={store.timeline} />
    {:else}
      <div class="w-full h-full border-2 border-dashed border-[var(--color-border)] rounded-2xl flex flex-col items-center justify-center opacity-50">
        <div class="text-xl font-medium text-[var(--color-text-muted)] mb-2">Loading Timeline...</div>
      </div>
    {/if}
  </section>
</main>
