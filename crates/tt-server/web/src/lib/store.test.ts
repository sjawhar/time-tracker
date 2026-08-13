import { beforeEach, describe, expect, it, vi } from 'vitest';
import * as api from './api';
import { createStatusStore } from './store.svelte';
import type { Verdict } from './types';

vi.mock('./api', () => ({
  fetchStatus: vi.fn(),
  fetchReport: vi.fn(),
  fetchTimeline: vi.fn(),
  fetchTodos: vi.fn(),
  fetchSessions: vi.fn(),
  fetchProposals: vi.fn(),
  subscribeToStatus: vi.fn(),
}));

describe('createStatusStore', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    vi.useFakeTimers();
  });

  it('initializes with loading state', () => {
    const store = createStatusStore();
    expect(store.loading).toBe(true);
    expect(store.verdict).toBeNull();
    expect(store.error).toBeNull();
  });

  it('loads verdict successfully', async () => {
    const mockVerdict: Verdict = {
      current_stream: null,
      top_todo: null,
      aligned: null,
      wip: { in_flight: [], limit: 1, wind_down_candidate: null },
      alignment_share: null,
      pending_proposals: 0,
      machines: [],
      classifier: {
        last_success_at: null,
        last_failure_at: null,
        last_error: null,
        consecutive_failures: 0,
      },
    };

    vi.mocked(api.fetchStatus).mockResolvedValue(mockVerdict);
    vi.mocked(api.fetchTimeline).mockResolvedValue({
      window: { start: '2026-07-25T00:00:00Z', end: '2026-07-25T24:00:00Z' },
      streams_active: [],
      idle_gaps: [],
      db_version: 1,
    });
    vi.mocked(api.fetchReport).mockResolvedValue({
      generated_at: '2026-07-25T00:00:00Z',
      timezone: 'Etc/UTC',
      week_start_day: 'monday',
      period: { start: '2026-07-25', end: '2026-07-25', type: 'day' },
      by_tag: [],
      streams: [],
      untagged: { time_direct_ms: 0, time_delegated_ms: 0, streams: [] },
      agent_sessions: {
        total: 0,
        by_source: {},
        by_type: {},
        top_sessions: [],
      },
      totals: {
        time_direct_ms: 0,
        time_delegated_ms: 0,
        stream_count: 0,
        unassigned_direct_ms: 0,
        unassigned_delegated_ms: 0,
        total_tracked_ms: 0,
      },
    });
    vi.mocked(api.fetchTodos).mockResolvedValue({ todos: [] });
    vi.mocked(api.fetchSessions).mockResolvedValue({ sessions: [] });
    vi.mocked(api.fetchProposals).mockResolvedValue({
      proposals: [],
      total_pending: 0,
    });
    vi.mocked(api.subscribeToStatus).mockReturnValue(vi.fn());

    const store = createStatusStore();
    store.init();

    // Wait for promises to resolve
    await vi.advanceTimersByTimeAsync(0);

    expect(store.loading).toBe(false);
    expect(store.verdict).toEqual(mockVerdict);
    expect(store.error).toBeNull();
  });

  it('handles fetch error', async () => {
    vi.mocked(api.fetchStatus).mockRejectedValue(new Error('Network error'));
    vi.mocked(api.fetchTimeline).mockRejectedValue(new Error('Network error'));
    vi.mocked(api.fetchReport).mockRejectedValue(new Error('Network error'));
    vi.mocked(api.fetchTodos).mockRejectedValue(new Error('Network error'));
    vi.mocked(api.fetchSessions).mockRejectedValue(new Error('Network error'));
    vi.mocked(api.fetchProposals).mockRejectedValue(new Error('Network error'));
    vi.mocked(api.subscribeToStatus).mockReturnValue(vi.fn());

    const store = createStatusStore();
    store.init();

    await vi.advanceTimersByTimeAsync(0);

    expect(store.loading).toBe(false);
    expect(store.verdict).toBeNull();
    expect(store.error).toBe('Network error');
  });
});
