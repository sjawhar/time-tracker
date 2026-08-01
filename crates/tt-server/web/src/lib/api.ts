import type { Proposal, Session, TimelineData, Todo, Verdict } from './types';

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

export async function fetchProposals(): Promise<{ proposals: Proposal[] }> {
  const response = await fetch('/api/proposals');
  if (!response.ok) {
    throw new Error(`Failed to fetch proposals: ${response.statusText}`);
  }
  return response.json();
}

export function subscribeToStatus(onUpdate: () => void): () => void {
  const eventSource = new EventSource('/api/sse');

  eventSource.addEventListener('status_changed', () => {
    onUpdate();
  });
  eventSource.addEventListener('events_appended', () => {
    onUpdate();
  });

  return () => {
    eventSource.close();
  };
}
