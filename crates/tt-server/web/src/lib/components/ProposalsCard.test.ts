import { render, screen } from '@testing-library/svelte';
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
    render(ProposalsCard, { proposals: [] });
    expect(screen.getByText('No pending proposals')).toBeInTheDocument();
  });

  it('renders empty state when proposals is null', () => {
    render(ProposalsCard, { proposals: null });
    expect(screen.getByText('No pending proposals')).toBeInTheDocument();
  });

  it('renders proposals with correct formatting', () => {
    render(ProposalsCard, { proposals: mockProposals });

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

  it('renders disabled accept/reject buttons', () => {
    render(ProposalsCard, { proposals: mockProposals });

    const rejectButtons = screen.getAllByText('Reject');
    const acceptButtons = screen.getAllByText('Accept');

    expect(rejectButtons).toHaveLength(2);
    expect(acceptButtons).toHaveLength(2);

    expect(rejectButtons[0]).toBeDisabled();
    expect(acceptButtons[0]).toBeDisabled();
  });
});
