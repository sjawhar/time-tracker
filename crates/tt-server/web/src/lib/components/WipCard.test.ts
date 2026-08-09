import { render } from '@testing-library/svelte';
import { describe, expect, it } from 'vitest';
import type { WipStatus } from '../types';
import WipCard from './WipCard.svelte';

describe('WipCard', () => {
  it('renders normal state when under limit', () => {
    const wip: WipStatus = {
      in_flight: [
        { stream_id: '1', name: 'Stream 1', direct_ms: 0, delegated_ms: 0 },
      ],
      limit: 2,
      wind_down_candidate: null,
    };
    const { getByText, queryByText } = render(WipCard, { wip });

    expect(getByText('WIP (1/2)')).toBeInTheDocument();
    expect(getByText('• Stream 1')).toBeInTheDocument();
    expect(queryByText('Wind down candidate:')).not.toBeInTheDocument();
  });

  it('renders amber state and wind down candidate when over limit', () => {
    const wip: WipStatus = {
      in_flight: [
        { stream_id: '1', name: 'Stream 1', direct_ms: 0, delegated_ms: 0 },
        { stream_id: '2', name: 'Stream 2', direct_ms: 0, delegated_ms: 0 },
      ],
      limit: 1,
      wind_down_candidate: 'Stream 2',
    };
    const { getByText } = render(WipCard, { wip });

    expect(getByText('WIP (2/1)')).toBeInTheDocument();
    expect(getByText('Wind down candidate:')).toBeInTheDocument();
    expect(getByText('Stream 2')).toBeInTheDocument();
  });

  it('uses break-words instead of truncate for long text', () => {
    const wip: WipStatus = {
      in_flight: [
        {
          stream_id: '1',
          name: 'A very long stream name that should wrap instead of truncating with an ellipsis',
          direct_ms: 0,
          delegated_ms: 0,
        },
        {
          stream_id: '2',
          name: 'Another stream',
          direct_ms: 0,
          delegated_ms: 0,
        },
      ],
      limit: 1,
      wind_down_candidate: 'Another very long stream name that should wrap',
    };
    const { getByText } = render(WipCard, { wip });

    const streamEl = getByText(
      '• A very long stream name that should wrap instead of truncating with an ellipsis',
    );
    expect(streamEl).toHaveClass('break-words');
    expect(streamEl).not.toHaveClass('truncate');

    const candidateEl = getByText(
      'Another very long stream name that should wrap',
    );
    expect(candidateEl).toHaveClass('break-words');
    expect(candidateEl).not.toHaveClass('truncate');
  });
});
