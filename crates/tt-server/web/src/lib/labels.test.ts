import { describe, expect, it } from 'vitest';
import { fitRotatedLabel, ROTATED_LABEL_MAX_CHARS } from './labels';

describe('fitRotatedLabel', () => {
  it('leaves a label that fits untouched', () => {
    expect(fitRotatedLabel('devbox: infra')).toBe('devbox: infra');
    expect(fitRotatedLabel('a'.repeat(ROTATED_LABEL_MAX_CHARS))).toBe(
      'a'.repeat(ROTATED_LABEL_MAX_CHARS),
    );
  });

  it('marks a shortened label with an ellipsis rather than letting the viewport cut it', () => {
    // The bug this replaces: a rotated label grows away from the axis, so the SVG
    // viewport clips its FIRST characters. `inspect_scout eval-log SQL guard` rendered
    // as `nspect_scout eval-`, which reads as a different stream, not a shortened one.
    const result = fitRotatedLabel('inspect_scout eval-log SQL guard');
    expect(result.startsWith('inspect_scout')).toBe(true);
    expect(result.endsWith('\u2026')).toBe(true);
    expect(result.length).toBeLessThanOrEqual(ROTATED_LABEL_MAX_CHARS);
  });

  it('does not leave a dangling space before the ellipsis', () => {
    // 24 chars lands mid-space for this name; trimming keeps the ellipsis attached to a
    // word so the label does not read as "word ...".
    const result = fitRotatedLabel('agent-c core PRs and audit sweep');
    expect(result).not.toContain(' \u2026');
    expect(result.endsWith('\u2026')).toBe(true);
  });

  it('keeps one ellipsis only, never doubling it', () => {
    const result = fitRotatedLabel('x'.repeat(200));
    expect([...result].filter((c) => c === '\u2026')).toHaveLength(1);
  });
});
