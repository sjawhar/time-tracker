<script lang="ts">
import { computePosition, flip, offset, shift } from '@floating-ui/dom';
import * as d3Scale from 'd3-scale';
import * as d3Time from 'd3-time';
import * as d3TimeFormat from 'd3-time-format';
import { onDestroy, onMount } from 'svelte';
import { SvelteSet } from 'svelte/reactivity';
import { fitRotatedLabel } from '../labels';
import { type HitTarget, TimelineHitTester } from '../timeline/hit-test';
import { getStreamColor } from '../timeline/stream-color';
import { PiecewiseTimeScale } from '../timeline/time-scale';
import { buildTooltipContent } from '../timeline/tooltip';
import { createVirtualElement } from '../timeline/virtual-element';
import type { TimelineData, TimelineStream } from '../types';

let { data }: { data: TimelineData } = $props();

let container = $state<HTMLDivElement | undefined>(undefined);
let canvas = $state<HTMLCanvasElement | undefined>(undefined);
let svg = $state<SVGSVGElement | undefined>(undefined);

let width = $state(0);
let height = $state(0);
let hitTester = new TimelineHitTester();
let hoveredTarget = $state<HitTarget | null>(null);
let tooltipHtml = $state('');
let tooltipEl = $state<HTMLDivElement | undefined>(undefined);
let mouseX = $state(0);
let mouseY = $state(0);
let expandedGaps = $state(new SvelteSet<number>());

const MARGIN = { top: 40, right: 20, bottom: 20, left: 60 };

const COLUMN_GAP = 10;

// Column geometry first: the vertical scale depends on how much headroom the stream
// labels need, which depends on how wide the columns are.

// Column geometry is only meaningful once the container has been measured. `width`
// starts at 0, and the SVG header renders before the ResizeObserver fires, so without
// the `measured` guard below this yields a negative width and the browser rejects
// every <rect> (`attribute width: A negative value is not valid`).
let measured = $derived(width > MARGIN.left + MARGIN.right);

let columnWidth = $derived(
  measured && data.streams_active.length > 0
    ? (width -
        MARGIN.left -
        MARGIN.right -
        (data.streams_active.length - 1) * COLUMN_GAP) /
        data.streams_active.length
    : 0,
);

let headerHeight = $derived(MARGIN.top);

let yScale = $derived(
  new PiecewiseTimeScale(
    [new Date(data.window.end), new Date(data.window.start)],
    [headerHeight, height - MARGIN.bottom],
    data.idle_gaps || [],
    expandedGaps,
  ),
);

// Canvas 2D cannot resolve CSS custom properties: assigning `var(--x)` to fillStyle
// is an invalid colour, which the browser silently ignores, leaving the PREVIOUS
// fill in place (this is why idle folds rendered in the last stream's colour).
// Resolve variables against the document once per draw instead.
function cssVar(name: string, fallback: string): string {
  if (typeof document === 'undefined') return fallback;
  const v = getComputedStyle(document.documentElement)
    .getPropertyValue(name)
    .trim();
  return v || fallback;
}

// Draw function
function draw() {
  if (!canvas || !width || !height) return;

  const ctx = canvas.getContext('2d');
  if (!ctx) return;

  // Handle high DPI displays
  const dpr = window.devicePixelRatio || 1;
  canvas.width = width * dpr;
  canvas.height = height * dpr;
  ctx.scale(dpr, dpr);

  ctx.clearRect(0, 0, width, height);
  // Rebuild hit tester
  hitTester = new TimelineHitTester();

  // Draw streams
  data.streams_active.forEach((streamData, i) => {
    const x = MARGIN.left + i * (columnWidth + COLUMN_GAP);
    const color = getStreamColor(streamData.stream, i);

    // Draw delegated intervals (faded)
    ctx.fillStyle = color;
    ctx.globalAlpha = 0.3;
    streamData.delegated_intervals.forEach((interval) => {
      const yStart = yScale.scale(new Date(interval.end)); // end is top
      const yEnd = yScale.scale(new Date(interval.start)); // start is bottom
      ctx.fillRect(x, yStart, columnWidth, yEnd - yStart);
    });

    // Add to hit tester
    streamData.delegated_intervals.forEach((interval) => {
      const yStart = yScale.scale(new Date(interval.end));
      const yEnd = yScale.scale(new Date(interval.start));
      hitTester.addInterval(
        interval,
        streamData.stream,
        true,
        x,
        yStart,
        yEnd,
        columnWidth,
      );
    });

    streamData.focus_intervals.forEach((interval) => {
      const yStart = yScale.scale(new Date(interval.end));
      const yEnd = yScale.scale(new Date(interval.start));
      hitTester.addInterval(
        interval,
        streamData.stream,
        false,
        x,
        yStart,
        yEnd,
        columnWidth,
      );
    });

    // Draw focus intervals (solid)
    ctx.globalAlpha = 1.0;
    streamData.focus_intervals.forEach((interval) => {
      const yStart = yScale.scale(new Date(interval.end));
      const yEnd = yScale.scale(new Date(interval.start));
      ctx.fillRect(x, yStart, columnWidth, yEnd - yStart);
    });

    // Draw events
    streamData.events.forEach((event) => {
      const y = yScale.scale(new Date(event.timestamp));
      hitTester.addEvent(event, streamData.stream, x + columnWidth / 2, y);

      if (event.kind === 'user_message') {
        ctx.beginPath();
        ctx.arc(x + columnWidth / 2, y, 4, 0, 2 * Math.PI);
        ctx.fillStyle = 'white';
        ctx.fill();
        ctx.strokeStyle = color;
        ctx.lineWidth = 2;
        ctx.stroke();
      } else if (event.kind === 'subagent_start') {
        ctx.beginPath();
        ctx.moveTo(x, y);
        ctx.lineTo(x + 8, y);
        ctx.strokeStyle = 'white';
        ctx.lineWidth = 2;
        ctx.stroke();
      } else if (event.kind === 'session_start') {
        ctx.beginPath();
        ctx.moveTo(x + columnWidth / 2 - 5, y + 5);
        ctx.lineTo(x + columnWidth / 2 + 5, y + 5);
        ctx.lineTo(x + columnWidth / 2, y - 5);
        ctx.fillStyle = 'white';
        ctx.fill();
      } else if (event.kind === 'session_end') {
        ctx.beginPath();
        ctx.moveTo(x + columnWidth / 2 - 5, y - 5);
        ctx.lineTo(x + columnWidth / 2 + 5, y - 5);
        ctx.lineTo(x + columnWidth / 2, y + 5);
        ctx.fillStyle = 'white';
        ctx.fill();
      }

      if (event.todo_linked) {
        // Draw link glyph
        ctx.beginPath();
        ctx.arc(x + columnWidth - 6, y, 3, 0, 2 * Math.PI);
        ctx.fillStyle = cssVar('--color-status-green', '#22c55e');
        ctx.fill();
      }
    });
  });

  // Draw folds
  const collapsedGaps = yScale.getCollapsedGaps();
  collapsedGaps.forEach((gap) => {
    const yTop = gap.yTop;
    const yBottom = gap.yBottom;
    const foldHeight = yBottom - yTop;

    // Draw hatched background
    ctx.save();
    ctx.beginPath();
    ctx.rect(MARGIN.left, yTop, width - MARGIN.left - MARGIN.right, foldHeight);
    ctx.clip();

    ctx.fillStyle = cssVar('--color-bg-surface', '#171717');
    ctx.fill();

    ctx.strokeStyle = cssVar('--color-border', '#262626');
    ctx.lineWidth = 1;
    for (let i = -width; i < width + foldHeight; i += 10) {
      ctx.beginPath();
      ctx.moveTo(MARGIN.left + i, yBottom);
      ctx.lineTo(MARGIN.left + i + foldHeight, yTop);
      ctx.stroke();
    }
    ctx.restore();

    // Draw borders
    ctx.beginPath();
    ctx.moveTo(MARGIN.left, yTop);
    ctx.lineTo(width - MARGIN.right, yTop);
    ctx.moveTo(MARGIN.left, yBottom);
    ctx.lineTo(width - MARGIN.right, yBottom);
    ctx.strokeStyle = cssVar('--color-border', '#262626');
    ctx.stroke();

    // Draw label
    const startStr = formatHour(new Date(gap.startMs));
    const endStr = formatHour(new Date(gap.endMs));
    const hours = (gap.durationMinutes / 60).toFixed(1);
    const label = `// idle ${hours} h · ${startStr} → ${endStr}`;

    ctx.fillStyle = cssVar('--color-text-muted', '#a3a3a3');
    ctx.font = '10px sans-serif';
    ctx.textAlign = 'center';
    ctx.textBaseline = 'middle';
    ctx.fillText(
      label,
      MARGIN.left + (width - MARGIN.left - MARGIN.right) / 2,
      yTop + foldHeight / 2,
    );
  });

  hitTester.build();
}

// Resize observer
let resizeObserver: ResizeObserver;

onMount(() => {
  resizeObserver = new ResizeObserver((entries) => {
    for (let entry of entries) {
      width = entry.contentRect.width;
      height = entry.contentRect.height;
    }
  });

  if (container) {
    resizeObserver.observe(container);
  }
});

onDestroy(() => {
  if (resizeObserver) {
    resizeObserver.disconnect();
  }
});

// Redraw when data or dimensions change
$effect(() => {
  draw();
});
// Tooltip positioning
$effect(() => {
  if (hoveredTarget && tooltipEl && container) {
    const containerRect = container.getBoundingClientRect();
    // The virtual element needs coordinates relative to the viewport
    const virtualEl = createVirtualElement(
      containerRect.left + mouseX,
      containerRect.top + mouseY,
    );

    const el = tooltipEl;
    if (!el) return;
    computePosition(virtualEl, el, {
      placement: 'right',
      middleware: [offset(10), flip(), shift({ padding: 8 })],
    }).then(({ x, y }) => {
      Object.assign(el.style, {
        left: `${x}px`,
        top: `${y}px`,
      });
    });
  }
});

function handleMouseMove(e: MouseEvent) {
  if (!container) return;
  const rect = container.getBoundingClientRect();
  mouseX = e.clientX - rect.left;
  mouseY = e.clientY - rect.top;

  const target = hitTester.find(mouseX, mouseY, 10);
  if (target) {
    hoveredTarget = target;
    tooltipHtml = buildTooltipContent(target);
    if (canvas) canvas.style.cursor = 'pointer';
  } else {
    hoveredTarget = null;
    if (canvas) canvas.style.cursor = 'default';
  }
}
function handleMouseLeave() {
  hoveredTarget = null;
  if (canvas) canvas.style.cursor = 'default';
}

function handleClick(e: MouseEvent) {
  if (!container) return;
  const rect = container.getBoundingClientRect();
  const y = e.clientY - rect.top;

  // Check if clicked on a collapsed gap
  const collapsedGaps = yScale.getCollapsedGaps();
  for (const gap of collapsedGaps) {
    if (y >= gap.yTop && y <= gap.yBottom) {
      expandedGaps.add(gap.index);
      return;
    }
  }

  // Check if clicked on an expanded gap to collapse it
  const t = yScale.invert(y).getTime();
  for (let i = 0; i < (data.idle_gaps || []).length; i++) {
    if (expandedGaps.has(i)) {
      const gap = data.idle_gaps[i];
      const startMs = new Date(gap.start).getTime();
      const endMs = new Date(gap.end).getTime();
      if (t >= startMs && t <= endMs) {
        expandedGaps.delete(i);
        return;
      }
    }
  }
}

// SVG Axis. A fixed hourly interval smears into an unreadable column on long windows
// (a 7-day view produced 168 overlapping labels), so the tick interval scales with the
// window span.
let windowSpanHours = $derived(
  (new Date(data.window.end).getTime() -
    new Date(data.window.start).getTime()) /
    3_600_000,
);

let tickInterval = $derived(
  windowSpanHours > 72
    ? d3Time.timeDay
    : windowSpanHours > 36
      ? d3Time.timeHour.every(6)
      : windowSpanHours > 12
        ? d3Time.timeHour.every(3)
        : d3Time.timeHour,
);

let yTicks = $derived(
  tickInterval
    ? yScale.getTicks(tickInterval)
    : yScale.getTicks(d3Time.timeHour),
);
const formatHour = d3TimeFormat.timeFormat('%H:%M');
const formatDay = d3TimeFormat.timeFormat('%b %d');
</script>

<div bind:this={container} class="relative w-full h-full" role="presentation" onmousemove={handleMouseMove} onmouseleave={handleMouseLeave} onclick={handleClick}>
  <canvas 
    bind:this={canvas} 
    class="absolute inset-0 pointer-events-none"
    style="width: {width}px; height: {height}px;"
  ></canvas>
  
  <svg 
    bind:this={svg} 
    class="absolute inset-0 pointer-events-none"
    {width} 
    {height}
  >
    <!-- Y Axis -->
    <g transform="translate({MARGIN.left - 10}, 0)">
      {#each yTicks as tick}
        {@const y = yScale.scale(tick)}
        {@const isDayBoundary = tick.getHours() === 0 && tick.getMinutes() === 0}
        <text 
          x="0" 
          {y} 
          dy="0.32em" 
          text-anchor="end" 
          class="text-xs fill-[var(--color-text-muted)] {isDayBoundary ? 'font-bold fill-[var(--color-text)]' : ''}"
        >
          {isDayBoundary ? formatDay(tick) : formatHour(tick)}
        </text>
        <line 
          x1="5" 
          x2="10" 
          y1={y} 
          y2={y} 
          class="stroke-[var(--color-border)]" 
        />
      {/each}
    </g>
    
  </svg>
  {#if hoveredTarget}
    <div
      bind:this={tooltipEl}
      class="fixed z-50 bg-[var(--color-bg-elevated)] border border-[var(--color-border)] rounded-md shadow-lg p-3 pointer-events-none max-w-xs"
    >
      <!-- eslint-disable-next-line svelte/no-at-html-tags -->
      {@html tooltipHtml}
    </div>
  {/if}

</div>
