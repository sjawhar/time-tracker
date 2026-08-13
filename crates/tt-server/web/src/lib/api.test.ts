import { describe, expect, it, vi } from 'vitest';
import { updateStream } from './api';

describe('api', () => {
  describe('updateStream', () => {
    it('constructs the correct PATCH payload', async () => {
      const fetchMock = vi.fn().mockResolvedValue({
        ok: true,
        json: () => Promise.resolve({ id: 's1', tags: [] }),
      });
      globalThis.fetch = fetchMock as unknown as typeof fetch;

      await updateStream('s1', {
        name: 'New Name',
        add_tags: ['tag1'],
        remove_tags: ['tag2'],
      });

      expect(fetchMock).toHaveBeenCalledWith('/api/streams/s1', {
        method: 'PATCH',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          name: 'New Name',
          add_tags: ['tag1'],
          remove_tags: ['tag2'],
        }),
      });
    });
  });
  describe('setTodoStream', () => {
    it('constructs the correct POST payload for setting a stream', async () => {
      const fetchMock = vi.fn().mockResolvedValue({
        ok: true,
        json: () =>
          Promise.resolve({
            todo_id: 't1',
            stream_slug: 's1',
            status: 'updated',
          }),
      });
      globalThis.fetch = fetchMock as unknown as typeof fetch;

      const { setTodoStream } = await import('./api');
      await setTodoStream('t1', 's1');

      expect(fetchMock).toHaveBeenCalledWith('/api/todos/t1/stream', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ stream: 's1' }),
      });
    });

    it('constructs the correct POST payload for clearing a stream', async () => {
      const fetchMock = vi.fn().mockResolvedValue({
        ok: true,
        json: () =>
          Promise.resolve({
            todo_id: 't1',
            stream_slug: null,
            status: 'updated',
          }),
      });
      globalThis.fetch = fetchMock as unknown as typeof fetch;

      const { setTodoStream } = await import('./api');
      await setTodoStream('t1', null);

      expect(fetchMock).toHaveBeenCalledWith('/api/todos/t1/stream', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ stream: null }),
      });
    });
  });
});
