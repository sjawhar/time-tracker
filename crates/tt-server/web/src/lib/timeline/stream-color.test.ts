import { describe, expect, it } from 'vitest';
import { getStreamColor, STREAM_COLORS } from './stream-color';

// This replaces the colour test that used to live in Timeline.test.ts, which re-declared
// getStreamColor inside the test file. That version asserted the behaviour of a copy, so
// it would have kept passing had the component's real function changed or broken. These
// import the function the component actually calls.
describe('getStreamColor', () => {
  it("prefers the stream's own colour", () => {
    expect(getStreamColor({ color: '#ff0000' }, 0)).toBe('#ff0000');
    // Index is ignored entirely when a colour is set.
    expect(getStreamColor({ color: '#ff0000' }, 7)).toBe('#ff0000');
  });

  it('falls back to the palette by index', () => {
    expect(getStreamColor({}, 0)).toBe(STREAM_COLORS[0]);
    expect(getStreamColor({}, 1)).toBe(STREAM_COLORS[1]);
  });

  it('wraps around the palette so any index yields a colour', () => {
    expect(getStreamColor({}, STREAM_COLORS.length)).toBe(STREAM_COLORS[0]);
    expect(getStreamColor({}, STREAM_COLORS.length * 3 + 2)).toBe(
      STREAM_COLORS[2],
    );
  });

  it('treats an absent or null colour the same as unset', () => {
    // The API models colour as nullable, so null must not be returned as a fill.
    expect(getStreamColor({ color: null }, 1)).toBe(STREAM_COLORS[1]);
    expect(getStreamColor({ color: '' }, 1)).toBe(STREAM_COLORS[1]);
  });
});
