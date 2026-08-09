/**
 * A rotated timeline label runs upward out of a fixed headroom, so anything longer than
 * that headroom is clipped by the SVG viewport -- and it loses its FIRST characters,
 * because the text is anchored at the axis and grows away from it. That is worse than a
 * short label: `inspect_scout eval-log` rendered as `nspect_scout eval-`, which reads as
 * a different stream rather than as a shortened one.
 *
 * Only 25 of ~1,280 streams carry a slug, so a column label is usually the full display
 * name and this is the common case rather than an edge one. Truncating explicitly makes
 * the shortening visible, and the full name stays reachable because both label branches
 * render an SVG `<title>`, which browsers show on hover.
 */
export const ROTATED_LABEL_MAX_CHARS = 24;

/**
 * Shortens `text` to fit the rotated-label headroom, marking the cut with an ellipsis.
 *
 * ~6px per glyph at text-xs (12px) against 150px of headroom fits ~24 glyphs plus the
 * ellipsis. Measuring precisely needs a canvas or `getComputedTextLength`, which is not
 * worth a layout pass for a label whose exact fit does not matter -- what matters is that
 * it stops before the viewport edge instead of being silently cut there.
 */
export function fitRotatedLabel(text: string): string {
  if (text.length <= ROTATED_LABEL_MAX_CHARS) {
    return text;
  }
  return `${text.slice(0, ROTATED_LABEL_MAX_CHARS - 1).trimEnd()}\u2026`;
}
