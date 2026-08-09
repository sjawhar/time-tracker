import { describe, expect, it } from 'vitest';
import type { TimelineStream } from '../types';
import type { HitTarget } from './hit-test';
import { buildTooltipContent } from './tooltip';

describe('buildTooltipContent', () => {
  const mockStream = { name: 'Test Stream' } as TimelineStream['stream'];

  it('formats event tooltip', () => {
    const target: HitTarget = {
      type: 'event',
      event: {
        timestamp: '2023-01-01T10:30:45Z',
        kind: 'user_message',
        session_id: 'ses_123',
        todo_linked: true,
      },
      stream: mockStream,
      x: 0,
      y: 0,
    };

    const html = buildTooltipContent(target);
    expect(html).toContain('User Message');
    expect(html).toContain('Test Stream');
    expect(html).toContain('ses_123');
    expect(html).toContain('Linked to Todo');
  });

  it('formats interval tooltip', () => {
    const target: HitTarget = {
      type: 'interval',
      interval: {
        start: '2023-01-01T10:00:00Z',
        end: '2023-01-01T11:30:00Z',
      },
      stream: mockStream,
      isDelegated: false,
      x: 0,
      yStart: 0,
      yEnd: 0,
      width: 0,
    };

    const html = buildTooltipContent(target);
    expect(html).toContain('Test Stream');
    expect(html).toContain('Direct Focus');
    expect(html).toContain('1h 30m');
  });
});
