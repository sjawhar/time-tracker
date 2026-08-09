import * as d3ScaleChromatic from 'd3-scale-chromatic';

/**
 * The palette timeline columns fall back to when a stream carries no explicit colour.
 */
export const STREAM_COLORS = d3ScaleChromatic.schemeTableau10;

/**
 * Picks a stream's column colour.
 *
 * A stream's own `color` wins when it has one; otherwise the palette is indexed by the
 * stream's position, so a given column keeps its colour for as long as the ordering
 * holds. Lives here rather than inline in `Timeline.svelte` so its test can import the
 * real function — the previous test re-declared this logic in the test file, which means
 * it would have kept passing had the component's version changed or broken.
 */
export function getStreamColor(
  stream: { color?: string | null },
  index: number,
): string {
  if (stream.color) {
    return stream.color;
  }
  return STREAM_COLORS[index % STREAM_COLORS.length];
}
