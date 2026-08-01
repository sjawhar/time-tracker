import {
  fetchProposals,
  fetchSessions,
  fetchStatus,
  fetchTimeline,
  fetchTodos,
  subscribeToStatus,
} from './api';
import type { Proposal, Session, TimelineData, Todo, Verdict } from './types';

export function createStatusStore() {
  let verdict = $state<Verdict | null>(null);
  let timeline = $state<TimelineData | null>(null);
  let todos = $state<Todo[] | null>(null);
  let sessions = $state<Session[] | null>(null);
  let proposals = $state<Proposal[] | null>(null);
  let error = $state<string | null>(null);
  let loading = $state(true);

  async function load() {
    try {
      const [v, t, td, s, p] = await Promise.all([
        fetchStatus(),
        fetchTimeline(),
        fetchTodos(),
        fetchSessions(),
        fetchProposals(),
      ]);
      verdict = v;
      timeline = t;
      todos = td.todos;
      sessions = s.sessions;
      proposals = p.proposals;
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
    unsubscribe = subscribeToStatus(load);

    return () => {
      clearInterval(interval);
      if (unsubscribe) unsubscribe();
    };
  }

  return {
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
    init,
  };
}
