// Pure text helpers (no DOM): safe to import from tests and from any feature.

/**
 * Fit a hit line to a narrow column: drop indentation and, when the first match starts far
 * right, cut the prefix behind an ellipsis. Ranges are UTF-16 offsets and shift with the text.
 * @param {string} text
 * @param {number[][]} [ranges]
 * @param {number} [lead] characters of context kept before the first match
 */
export function focusHit(text, ranges = [], lead = 16) {
  let t = text.replace(/\s+$/, '');
  let cut = t.length - t.trimStart().length;
  const first = ranges.length ? ranges[0][0] : 0;
  if (first - cut > lead + 8) cut = first - lead;
  if (!cut) return { text: t, ranges };
  const prefix = cut > t.length - t.trimStart().length ? '…' : '';
  t = prefix + t.slice(cut);
  const shift = prefix.length - cut;
  return { text: t, ranges: ranges.map(([a, b]) => [Math.max(prefix.length, a + shift), b + shift]).filter(([a, b]) => b > a) };
}
