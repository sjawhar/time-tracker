import { fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import { describe, expect, it, vi } from 'vitest';
import * as api from '../api';
import StreamPicker from './StreamPicker.svelte';

vi.mock('../api', () => ({
  listStreams: vi.fn(),
}));

describe('StreamPicker', () => {
  const mockStreams = [
    { id: '1', name: 'First Stream', slug: 'first-stream', last_active: null },
    {
      id: '2',
      name: 'Second Stream',
      slug: 'second-stream',
      last_active: null,
    },
    { id: '3', name: null, slug: 'third-stream', last_active: null },
  ];

  it('fetches and displays streams', async () => {
    vi.mocked(api.listStreams).mockResolvedValueOnce({ streams: mockStreams });

    render(StreamPicker, { onSelect: vi.fn(), onClose: vi.fn() });

    expect(screen.getByText('Loading streams...')).toBeInTheDocument();

    await waitFor(() => {
      expect(screen.getByText('first-stream')).toBeInTheDocument();
    });

    expect(screen.getByText('First Stream')).toBeInTheDocument();
    expect(screen.getByText('second-stream')).toBeInTheDocument();
    expect(screen.getByText('third-stream')).toBeInTheDocument();
  });

  it('filters streams by text', async () => {
    vi.mocked(api.listStreams).mockResolvedValueOnce({ streams: mockStreams });

    render(StreamPicker, { onSelect: vi.fn(), onClose: vi.fn() });

    await waitFor(() => {
      expect(screen.getByText('first-stream')).toBeInTheDocument();
    });

    const input = screen.getByPlaceholderText('Filter streams...');
    await fireEvent.input(input, { target: { value: 'second' } });

    expect(screen.queryByText('first-stream')).not.toBeInTheDocument();
    expect(screen.getByText('second-stream')).toBeInTheDocument();
  });

  it('calls onSelect when a stream is clicked', async () => {
    vi.mocked(api.listStreams).mockResolvedValueOnce({ streams: mockStreams });
    const onSelect = vi.fn();

    render(StreamPicker, { onSelect, onClose: vi.fn() });

    await waitFor(() => {
      expect(screen.getByText('first-stream')).toBeInTheDocument();
    });

    await fireEvent.click(screen.getByText('first-stream'));

    expect(onSelect).toHaveBeenCalledWith('1');
  });

  it('calls onSelect with top match on Enter', async () => {
    vi.mocked(api.listStreams).mockResolvedValueOnce({ streams: mockStreams });
    const onSelect = vi.fn();

    render(StreamPicker, { onSelect, onClose: vi.fn() });

    await waitFor(() => {
      expect(screen.getByText('first-stream')).toBeInTheDocument();
    });

    const input = screen.getByPlaceholderText('Filter streams...');
    await fireEvent.input(input, { target: { value: 'third' } });

    await fireEvent.keyDown(window, { key: 'Enter' });

    expect(onSelect).toHaveBeenCalledWith('3');
  });

  it('calls onClose on Escape', async () => {
    vi.mocked(api.listStreams).mockResolvedValueOnce({ streams: mockStreams });
    const onClose = vi.fn();

    render(StreamPicker, { onSelect: vi.fn(), onClose });

    await waitFor(() => {
      expect(screen.getByText('first-stream')).toBeInTheDocument();
    });

    await fireEvent.keyDown(window, { key: 'Escape' });

    expect(onClose).toHaveBeenCalled();
  });
});
