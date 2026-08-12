import { fireEvent, render, screen } from '@testing-library/svelte';
import { describe, expect, it } from 'vitest';
import type { Proposal } from '../types';
import ProposalsCard from './ProposalsCard.svelte';

describe('ProposalsCard', () => {
  const mockProposals: Proposal[] = [
    {
      id: '1',
      created_at: '2026-07-26T10:00:00Z',
      target: {
        kind: 'existing',
        name: 'Alpha Stream',
        slug: 'alpha',
        stream_id: 's1',
      },
      confidence: 0.85,
      reasoning: 'Matches previous work on alpha.',
      scope: { kind: 'session', count: 1 },
    },
    {
      id: '2',
      created_at: '2026-07-26T10:05:00Z',
      target: { kind: 'new', name: 'Beta Feature', description: 'New feature' },
      confidence: 0.65,
      reasoning: 'Looks like a new feature.',
      scope: { kind: 'events', count: 5 },
    },
  ];

  it('renders empty state when no proposals', () => {
    render(ProposalsCard, {
      proposalsData: { items: [], total_pending: 0 },
      onDecide: () => {},
    });
    expect(screen.getByText('No pending proposals')).toBeInTheDocument();
  });

  it('renders empty state when proposals is null', () => {
    render(ProposalsCard, { proposalsData: null, onDecide: () => {} });
    expect(screen.getByText('No pending proposals')).toBeInTheDocument();
  });

  it('renders proposals with correct formatting', () => {
    render(ProposalsCard, {
      proposalsData: { items: mockProposals, total_pending: 2 },
      onDecide: () => {},
    });

    expect(screen.getByText('Alpha Stream')).toBeInTheDocument();
    expect(screen.getByText('85%')).toBeInTheDocument();
    expect(
      screen.getByText('Matches previous work on alpha.'),
    ).toBeInTheDocument();

    expect(screen.getByText('New:')).toBeInTheDocument();
    expect(screen.getByText('Beta Feature')).toBeInTheDocument();
    expect(screen.getByText('65%')).toBeInTheDocument();
    expect(screen.getByText('Looks like a new feature.')).toBeInTheDocument();
  });

  it('calls onDecide with the proposal id and the verdict', async () => {
    const calls: Array<[string, string]> = [];
    render(ProposalsCard, {
      proposalsData: { items: mockProposals, total_pending: 2 },
      onDecide: (id: string, decision: 'accept' | 'reject') => {
        calls.push([id, decision]);
      },
    });

    const rejectButtons = screen.getAllByText('Reject');
    const acceptButtons = screen.getAllByText('Accept');
    expect(rejectButtons).toHaveLength(2);
    expect(acceptButtons).toHaveLength(2);
    expect(rejectButtons[0]).toBeEnabled();
    expect(acceptButtons[0]).toBeEnabled();

    await fireEvent.click(acceptButtons[0]);
    await fireEvent.click(rejectButtons[1]);

    expect(calls).toEqual([
      ['1', 'accept'],
      ['2', 'reject'],
    ]);
  });

  it('locks every button while a decision is in flight', () => {
    // Accepting writes `assignment_source = 'user'`, a verdict no machine writer
    // overwrites, so a double-click must not be able to send a second one.
    render(ProposalsCard, {
      proposalsData: { items: mockProposals, total_pending: 2 },
      deciding: '1',
      onDecide: () => {},
    });

    expect(screen.getByText('Accepting...')).toBeInTheDocument();
    for (const b of [...screen.getAllByRole('button')]) {
      expect(b).toBeDisabled();
    }
  });
});
