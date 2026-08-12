import {
  acceptProposal,
  fetchProposals,
  fetchReport,
  fetchSessions,
  fetchStatus,
  fetchTimeline,
  fetchTodos,
  rejectProposal,
  subscribeToStatus,
} from './api';
import type {
  Proposal,
  Report,
  Session,
  TimelineData,
  Todo,
  Verdict,
} from './types';

export function createStatusStore() {
  let verdict = $state<Verdict | null>(null);
  let timeline = $state<TimelineData | null>(null);
  let report = $state<Report | null>(null);
  let todos = $state<Todo[] | null>(null);
  let sessions = $state<Session[] | null>(null);
  let proposals = $state<{ items: Proposal[]; total_pending: number } | null>(
    null,
  );
  let error = $state<string | null>(null);
  let loading = $state(true);
  let reportPeriod = $state<'day' | 'week'>('day');
  let decidingProposal = $state<string | null>(null);

  let loadTimeout: ReturnType<typeof setTimeout> | null = null;
  let pendingEvents = new Set<'status_changed' | 'events_appended'>();

  function handleUpdate(event: 'status_changed' | 'events_appended') {
    pendingEvents.add(event);
    if (loadTimeout) clearTimeout(loadTimeout);
    loadTimeout = setTimeout(() => {
      const events = new Set(pendingEvents);
      pendingEvents.clear();
      fetchUpdates(events);
    }, 100);
  }

  async function fetchUpdates(
    events: Set<'status_changed' | 'events_appended'>,
  ) {
    try {
      const promises: Promise<void>[] = [];

      if (events.has('status_changed')) {
        promises.push(
          fetchStatus().then((v) => {
            verdict = v;
          }),
          fetchTodos().then((td) => {
            todos = td.todos;
          }),
          fetchProposals().then((p) => {
            proposals = { items: p.proposals, total_pending: p.total_pending };
          }),
        );
      }

      if (events.has('status_changed') || events.has('events_appended')) {
        promises.push(
          fetchTimeline().then((t) => {
            timeline = t;
          }),
          (() => {
            // Capture the period this request was issued for. A background refresh
            // and a user's toggle can be in flight together, and responses are not
            // ordered: without this the slower one wins and silently reverts the view.
            const requested = reportPeriod;
            return fetchReport(requested).then((r) => {
              if (reportPeriod === requested) {
                report = r;
              }
            });
          })(),
          fetchSessions().then((s) => {
            sessions = s.sessions;
          }),
        );
      }

      await Promise.all(promises);
      error = null;
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    }
  }

  async function load() {
    try {
      const [v, t, r, td, s, p] = await Promise.all([
        fetchStatus(),
        fetchTimeline(),
        fetchReport(reportPeriod),
        fetchTodos(),
        fetchSessions(),
        fetchProposals(),
      ]);
      verdict = v;
      timeline = t;
      report = r;
      todos = td.todos;
      sessions = s.sessions;
      proposals = { items: p.proposals, total_pending: p.total_pending };
      error = null;
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      loading = false;
    }
  }

  let unsubscribe: (() => void) | null = null;

  function init() {
    load();
    const interval = setInterval(load, 60000); // Poll every minute as fallback
    unsubscribe = subscribeToStatus(handleUpdate);

    return () => {
      clearInterval(interval);
      if (unsubscribe) unsubscribe();
    };
  }

  return {
    get report() {
      return report;
    },
    get verdict() {
      return verdict;
    },
    get timeline() {
      return timeline;
    },
    get todos() {
      return todos;
    },
    get sessions() {
      return sessions;
    },
    get proposals() {
      return proposals;
    },
    get error() {
      return error;
    },
    get loading() {
      return loading;
    },
    get reportPeriod() {
      return reportPeriod;
    },
    setReportPeriod(period: 'day' | 'week') {
      reportPeriod = period;
      fetchReport(period).then((r) => {
        // Toggling twice quickly leaves two requests outstanding; apply a response
        // only while it still describes what the user is looking at.
        if (reportPeriod === period) {
          report = r;
        }
      });
    },
    get decidingProposal() {
      return decidingProposal;
    },
    /**
     * Records a human verdict on a proposal and refreshes what it changed.
     *
     * Accepting writes `assignment_source = 'user'`, which no machine writer will
     * overwrite, so exactly one decision is in flight at a time: a double-click must
     * not send two verdicts. Both the queue and the report are refetched, because
     * accepting moves events onto a stream and so changes the figures the TIME panel
     * is showing. A failure is surfaced rather than swallowed -- the server answers
     * 409 when the queue moved underneath the reviewer, and that is worth reading.
     */
    async decideProposal(id: string, decision: 'accept' | 'reject') {
      if (decidingProposal) return;
      decidingProposal = id;
      try {
        if (decision === 'accept') {
          await acceptProposal(id);
        } else {
          await rejectProposal(id);
        }
        const [p, r] = await Promise.all([
          fetchProposals(),
          fetchReport(reportPeriod),
        ]);
        proposals = { items: p.proposals, total_pending: p.total_pending };
        report = r;
        error = null;
      } catch (e) {
        error = e instanceof Error ? e.message : String(e);
      } finally {
        decidingProposal = null;
      }
    },
    init,
  };
}
