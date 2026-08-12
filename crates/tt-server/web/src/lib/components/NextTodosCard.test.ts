import { fireEvent, render, screen } from '@testing-library/svelte';
import { describe, expect, it } from 'vitest';
import type { Todo } from '../types';
import NextTodosCard from './NextTodosCard.svelte';

describe('NextTodosCard', () => {
  const mockTodos: Todo[] = [
    {
      id: '1',
      text: 'First todo',
      section: 'main',
      priorities: [],
      stream_slug: 'stream-1',
      due: null,
      when: null,
      linked_agent_count: 2,
    },
    {
      id: '2',
      text: 'Second todo',
      section: 'main',
      priorities: [],
      stream_slug: null,
      due: null,
      when: null,
      linked_agent_count: 0,
    },
    {
      id: '3',
      text: 'Third todo',
      section: 'main',
      priorities: [],
      stream_slug: 'stream-3',
      due: null,
      when: null,
      linked_agent_count: 1,
    },
    {
      id: '4',
      text: 'Fourth todo',
      section: 'main',
      priorities: [],
      stream_slug: 'stream-4',
      due: null,
      when: null,
      linked_agent_count: 0,
    },
  ];

  it('renders empty state when no todos', () => {
    render(NextTodosCard, { todos: [] });
    expect(screen.getByText('No pending todos')).toBeInTheDocument();
  });

  it('renders empty state when todos is null', () => {
    render(NextTodosCard, { todos: null });
    expect(screen.getByText('No pending todos')).toBeInTheDocument();
  });

  it('renders up to 3 todos by default', () => {
    render(NextTodosCard, { todos: mockTodos });
    expect(screen.getByText('First todo')).toBeInTheDocument();
    expect(screen.getByText('Second todo')).toBeInTheDocument();
    expect(screen.getByText('Third todo')).toBeInTheDocument();
    expect(screen.queryByText('Fourth todo')).not.toBeInTheDocument();
  });

  it('shows agent count when linked_agent_count > 0', () => {
    render(NextTodosCard, { todos: mockTodos });
    expect(screen.getByText('2 agents running')).toBeInTheDocument();
    expect(screen.getByText('1 agent running')).toBeInTheDocument();
  });

  it('expands to show more todos when button is clicked', async () => {
    render(NextTodosCard, { todos: mockTodos });

    const button = screen.getByText('Show more (4)');
    await fireEvent.click(button);

    expect(screen.getByText('Fourth todo')).toBeInTheDocument();
    expect(screen.getByText('Show less')).toBeInTheDocument();
  });

  it('caps the expanded list so the page cannot grow without bound', async () => {
    // Unbounded panels are what rendered a 101,855px page. Expanding shows at
    // most 12 and reports the rest as a count, so the label is 'Show more'
    // rather than 'Show all' -- with a cap, 'all' would be untrue.
    const many: Todo[] = Array.from({ length: 20 }, (_, i) => ({
      id: String(i + 1),
      text: `Todo ${i + 1}`,
      section: 'main',
      priorities: [],
      stream_slug: null,
      due: null,
      when: null,
      linked_agent_count: 0,
    }));
    render(NextTodosCard, { todos: many });

    await fireEvent.click(screen.getByText('Show more (20)'));

    expect(screen.getByText('Todo 12')).toBeInTheDocument();
    expect(screen.queryByText('Todo 13')).not.toBeInTheDocument();
    expect(screen.getByText('+ 8 more pending')).toBeInTheDocument();
  });
});
