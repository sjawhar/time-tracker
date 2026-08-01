import { render } from '@testing-library/svelte';
import { describe, expect, it } from 'vitest';
import type { ClassifierHealth } from '../types';
import ClassifierHealthCard from './ClassifierHealthCard.svelte';

describe('ClassifierHealthCard', () => {
  it('renders healthy state', () => {
    const health: ClassifierHealth = {
      last_success_at: new Date().toISOString(),
      last_failure_at: null,
      last_error: null,
      consecutive_failures: 0,
    };
    const { getByText } = render(ClassifierHealthCard, { health });

    expect(getByText('Healthy')).toBeInTheDocument();
  });

  it('renders failing state with error', () => {
    const health: ClassifierHealth = {
      last_success_at: null,
      last_failure_at: new Date().toISOString(),
      last_error: 'API rate limit exceeded',
      consecutive_failures: 3,
    };
    const { getByText } = render(ClassifierHealthCard, { health });

    expect(
      getByText('Failing (3x) — API rate limit exceeded'),
    ).toBeInTheDocument();
  });

  it('parses JSON error messages', () => {
    const health: ClassifierHealth = {
      consecutive_failures: 5,
      last_error: JSON.stringify({ error: { message: 'Auth token expired' } }),
      last_success_at: null,
      last_failure_at: null,
    };
    const { getByText } = render(ClassifierHealthCard, { health });

    expect(getByText('Failing (5x) — Auth token expired')).toBeInTheDocument();
  });
});
