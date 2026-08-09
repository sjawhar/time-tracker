import { render } from '@testing-library/svelte';
import { describe, expect, it } from 'vitest';
import type { Verdict } from '../types';
import VerdictCard from './VerdictCard.svelte';

describe('VerdictCard', () => {
  const baseVerdict: Verdict = {
    current_stream: {
      stream_id: '1',
      name: 'Test Stream',
      since: new Date().toISOString(),
    },
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

  it('renders UNKNOWN state when aligned is null', () => {
    const { getByText } = render(VerdictCard, { verdict: baseVerdict });
    expect(getByText('UNKNOWN')).toBeInTheDocument();
  });

  it('renders ALIGNED state when aligned is true', () => {
    const { getByText } = render(VerdictCard, {
      verdict: { ...baseVerdict, aligned: true },
    });
    expect(getByText('ALIGNED')).toBeInTheDocument();
  });

  it('renders DRIFTING state when aligned is false', () => {
    const { getByText } = render(VerdictCard, {
      verdict: { ...baseVerdict, aligned: false },
    });
    expect(getByText('DRIFTING')).toBeInTheDocument();
  });

  it('renders "No active stream" when current_stream is null', () => {
    const { getByText } = render(VerdictCard, {
      verdict: { ...baseVerdict, current_stream: null },
    });
    expect(getByText('No active stream')).toBeInTheDocument();
  });

  it('applies green styling when ALIGNED', () => {
    const { getByText } = render(VerdictCard, {
      verdict: { ...baseVerdict, aligned: true },
    });
    const element = getByText('ALIGNED');
    expect(element).toHaveStyle({ color: 'var(--color-status-green)' });
    expect(element).toHaveClass('text-[min(3.75rem,18.5cqi)]');
    expect(element.parentElement).toHaveClass('@container');
  });

  it('applies red styling when DRIFTING', () => {
    const { getByText } = render(VerdictCard, {
      verdict: { ...baseVerdict, aligned: false },
    });
    const element = getByText('DRIFTING');
    expect(element).toHaveStyle({ color: 'var(--color-status-red)' });
    expect(element).toHaveClass('text-[min(3.75rem,18.5cqi)]');
    expect(element.parentElement).toHaveClass('@container');
  });
});
