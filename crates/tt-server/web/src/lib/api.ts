import type {
  Proposal,
  Report,
  Session,
  TimelineData,
  Todo,
  Verdict,
} from './types';

export async function fetchStatus(): Promise<Verdict> {
  const response = await fetch('/api/status');
  if (!response.ok) {
    throw new Error(`Failed to fetch status: ${response.statusText}`);
  }
  return response.json();
}

export async function fetchTimeline(
  duration: string = '24h',
): Promise<TimelineData> {
  const response = await fetch(`/api/timeline?duration=${duration}`);
  if (!response.ok) {
    throw new Error(`Failed to fetch timeline: ${response.statusText}`);
  }
  return response.json();
}
export async function fetchReport(period: string = 'week'): Promise<Report> {
  const response = await fetch(`/api/report?period=${period}`);
  if (!response.ok) {
    throw new Error(`Failed to fetch report: ${response.statusText}`);
  }
  return response.json();
}

export async function fetchTodos(): Promise<{ todos: Todo[] }> {
  const response = await fetch('/api/todos');
  if (!response.ok) {
    throw new Error(`Failed to fetch todos: ${response.statusText}`);
  }
  return response.json();
}

export async function fetchSessions(): Promise<{ sessions: Session[] }> {
  const response = await fetch('/api/sessions');
  if (!response.ok) {
    throw new Error(`Failed to fetch sessions: ${response.statusText}`);
  }
  return response.json();
}

export async function fetchProposals(): Promise<{
  proposals: Proposal[];
  total_pending: number;
}> {
  const response = await fetch('/api/proposals');
  if (!response.ok) {
    throw new Error(`Failed to fetch proposals: ${response.statusText}`);
  }
  return response.json();
}

/** Outcome of a human verdict on a proposal. */
export interface ProposalDecision {
  proposal_id: string;
  status: string;
  stream_id: string | null;
  created_stream?: boolean;
  events_assigned: number;
}

/**
 * Records a human verdict on a proposal.
 *
 * This is the only write the dashboard performs, and accepting writes
 * `assignment_source = 'user'` -- a verdict no machine writer will overwrite. The
 * server returns 409 when the proposal is no longer pending, which happens when a
 * later classifier verdict superseded it or another window already decided it; the
 * message is surfaced rather than swallowed so the reviewer learns the queue moved
 * underneath them instead of seeing a click do nothing.
 */
async function decideProposal(
  id: string,
  decision: 'accept' | 'reject',
): Promise<ProposalDecision> {
  const response = await fetch(
    `/api/proposals/${encodeURIComponent(id)}/${decision}`,
    {
      method: 'POST',
    },
  );
  if (!response.ok) {
    const detail = (await response.text()).trim();
    throw new Error(
      `Failed to ${decision} proposal: ${detail || response.statusText}`,
    );
  }
  return response.json();
}

export function acceptProposal(id: string): Promise<ProposalDecision> {
  return decideProposal(id, 'accept');
}

export function rejectProposal(id: string): Promise<ProposalDecision> {
  return decideProposal(id, 'reject');
}

export function subscribeToStatus(
  onUpdate: (event: 'status_changed' | 'events_appended') => void,
): () => void {
  const eventSource = new EventSource('/api/sse');

  eventSource.addEventListener('status_changed', () => {
    onUpdate('status_changed');
  });
  eventSource.addEventListener('events_appended', () => {
    onUpdate('events_appended');
  });

  return () => {
    eventSource.close();
  };
}
