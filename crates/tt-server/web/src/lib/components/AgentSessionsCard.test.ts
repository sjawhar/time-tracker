import { render, screen } from '@testing-library/svelte';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { Session } from '../types';
import AgentSessionsCard from './AgentSessionsCard.svelte';

describe('AgentSessionsCard', () => {
  beforeEach(() => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date('2026-07-26T10:00:00Z'));
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  const mockSessions: Session[] = [
    {
      harness: 'claude',
      session_id: '1',
      stream: { name: 'Alpha Stream', slug: 'alpha' },
      machine_label: 'devbox',
      start_time: '2026-07-26T09:00:00Z',
      duration_ms: 3600000, // 1h
      last_activity: '2026-07-26T09:55:00Z', // 5 mins ago (active)
      linked_todo_text: 'Fix the bug',
    },
    {
      harness: 'opencode',
      session_id: '2',
      stream: null,
      machine_label: null,
      start_time: '2026-07-26T09:00:00Z',
      duration_ms: 1800000, // 30m
      last_activity: '2026-07-26T09:45:00Z', // 15 mins ago (quiet)
      linked_todo_text: null,
    },
  ];

  it('renders empty state when no sessions', () => {
    render(AgentSessionsCard, { sessions: [] });
    expect(screen.getByText('No active sessions')).toBeInTheDocument();
  });

  it('renders empty state when sessions is null', () => {
    render(AgentSessionsCard, { sessions: null });
    expect(screen.getByText('No active sessions')).toBeInTheDocument();
  });

  it('renders sessions with correct formatting', () => {
    render(AgentSessionsCard, { sessions: mockSessions });

    expect(screen.getByText('claude')).toBeInTheDocument();
    expect(screen.getByText('devbox')).toBeInTheDocument();
    expect(screen.getByText('1h 0m')).toBeInTheDocument();
    expect(screen.getByText('Alpha Stream')).toBeInTheDocument();
    expect(screen.getByText('↳')).toBeInTheDocument();
    expect(screen.getByText('Fix the bug')).toBeInTheDocument();

    expect(screen.getByText('opencode')).toBeInTheDocument();
    expect(screen.getByText('30m')).toBeInTheDocument();
    expect(screen.getByText('Unclassified')).toBeInTheDocument();
    expect(screen.getByText('Unlinked')).toBeInTheDocument();
  });

  it('applies quiet styling to sessions inactive for >10 mins', () => {
    const { container } = render(AgentSessionsCard, { sessions: mockSessions });

    // The first session is active (5 mins ago)
    const activeIndicator = container.querySelector('[title="Active"]');
    expect(activeIndicator).toBeInTheDocument();

    // The second session is quiet (15 mins ago)
    const quietIndicator = container.querySelector(
      '[title="Quiet (>10m no activity)"]',
    );
    expect(quietIndicator).toBeInTheDocument();
  });
});
