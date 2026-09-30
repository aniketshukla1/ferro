// Word wrap for the code view (FRONTEND.md § 6.9): rows get measured heights (files ≤ 50k lines).
// Loaded on the first Alt+Z; the viewer keeps only the toggle.
import { heightModel } from '../core/heights.js';

// East Asian wide characters and emoji take two monospace columns.
const WIDE = /[ᄀ-ᅟ⺀-꓏가-힣豈-﫿︰-﹏＀-｠￠-￦\u{1F300}-\u{1FAFF}]/u;

/** Monospace columns a line occupies: tabs advance to the next stop, wide characters count 2. */
export function lineCols(line, tabSize = 4) {
  if (line.__cols != null) return line.__cols;
  const text = line.text ?? (line.html || '').replace(/<[^>]*>/g, '').replace(/&(?:#\d+|#x[0-9a-f]+|\w+);/gi, 'x');
  let cols = 0;
  for (const ch of text) {
    if (ch === '\t') cols += tabSize - (cols % tabSize);
    else cols += WIDE.test(ch) ? 2 : 1;
  }
  if (line.cut) cols += 40; // the "Show full line" button after a cut line
  return (line.__cols = cols);
}

/**
 * Turn wrap on for a code view. Heights come from loaded lines (unknown ones count one row);
 * `relayout()` re-measures after chunks load or the width changes, keeping the first visible line
 * in place. Returns { relayout, destroy }.
 */
export function wrapView({ vl, scroller, lh, lineAt, charWidth, extra = () => 0 }) {
  let cols = 80;
  const measureCols = () => {
    const gutter = parseFloat(scroller.style.getPropertyValue('--gutter-w')) || 60;
    // .cv-code pads 4px left and 56px right
    cols = Math.max(10, Math.floor((scroller.clientWidth - gutter - 60) / charWidth()));
  };
  const tab = parseInt(getComputedStyle(scroller).tabSize, 10) || 4;
  const model = heightModel((i) => {
    const line = lineAt(i + 1);
    // `extra`: space below a row for a box laid over it (the inline editor).
    return (line ? lh * Math.max(1, Math.ceil(lineCols(line, tab) / cols)) : lh) + extra(i);
  });

  function relayout(apply = () => vl.setHeights(model)) {
    const viewH = scroller.clientHeight;
    const { first } = vl.visibleRange();
    const into = vl.virtualTop(scroller.scrollTop, viewH) - vl.topPad - vl.rowTop(first);
    measureCols();
    apply();
    const vTop = vl.rowTop(first) + vl.topPad + Math.min(Math.max(0, into), vl.rowH(first));
    scroller.scrollTop = vl.scrollTopFor(vTop, viewH);
  }

  let width = scroller.clientWidth;
  const ro = new ResizeObserver(() => {
    if (scroller.clientWidth && scroller.clientWidth !== width) relayout();
    width = scroller.clientWidth;
  });
  ro.observe(scroller);
  relayout();

  return {
    relayout: () => relayout(),
    destroy() {
      ro.disconnect();
      relayout(() => vl.setHeights(null));
    },
  };
}
