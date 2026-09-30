// WCAG 2.1 contrast math and the § 5.2 floors every theme must meet. Shared by contrast.test.js
// and the theme generator (tools/gen-themes.mjs). Not a test file itself.
import { readFileSync } from 'node:fs';

function lum(hex) {
  const c = [1, 3, 5].map((i) => parseInt(hex.slice(i, i + 2), 16) / 255).map((x) => (x <= 0.03928 ? x / 12.92 : ((x + 0.055) / 1.055) ** 2.4));
  return 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
}

export function ratio(a, b) {
  const [x, y] = [lum(a), lum(b)].sort((p, q) => q - p);
  return (x + 0.05) / (y + 0.05);
}

/** `color-mix(in srgb, top pct%, transparent)` painted over an opaque base, as a hex color. */
export function blend(top, pct, base) {
  const ch = (hex, i) => parseInt(hex.slice(i, i + 2), 16);
  return '#' + [1, 3, 5].map((i) => Math.round(ch(top, i) * pct + ch(base, i) * (1 - pct)).toString(16).padStart(2, '0')).join('');
}

/** `[data-theme="id"] { --name: #rrggbb; … }` blocks of a stylesheet, as { id: { name: hex } }. */
export function parseThemes(css) {
  const out = {};
  for (const m of css.matchAll(/\[data-theme="([\w-]+)"\]\s*\{([^}]*)\}/g)) {
    const vars = {};
    for (const v of m[2].matchAll(/--([\w-]+):\s*(#[0-9a-fA-F]{6})\s*;/g)) vars[v[1]] = v[2].toLowerCase();
    out[m[1]] = vars;
  }
  return out;
}

export const readThemes = (...files) => Object.assign({}, ...files.map((f) => parseThemes(readFileSync(f, 'utf8'))));

export const SURFACES = ['bg-0', 'bg-chrome', 'bg-3'];
const SEMANTIC = ['ok', 'warn', 'danger', 'info'];

/**
 * Every rule is { fg, need, group, min(vars) } where min() is the lowest ratio of `fg` over the
 * rule's backgrounds. Mix percentages mirror the derived tokens at the top of themes.css.
 */
export function contrastRules(v) {
  const rules = [];
  const on = (fg, need, bgs, group) => rules.push({
    fg, need, group,
    min: (x) => Math.min(...bgs.map((bg) => ratio(x[fg], typeof bg === 'function' ? bg(x) : x[bg]))),
  });
  // § 5.2 text and accent floors
  for (const fg of ['fg', 'fg-muted']) on(fg, 4.5, SURFACES, 'text');
  on('fg-faint', 3, SURFACES, 'text');
  on('code-fg', 4.5, ['bg-0'], 'text');
  on('accent', 3, ['bg-0'], 'accent');
  // § 5.2 syntax floor, also on added / removed diff lines
  const addLine = (x) => blend(x.ok, 0.11, x['bg-0']);
  const delLine = (x) => blend(x.danger, 0.11, x['bg-0']);
  for (const k of Object.keys(v).filter((n) => n.startsWith('syn-'))) on(k, 3, ['bg-0', addLine, delLine], 'syntax');
  // semantic state as text or glyphs: git letters (chrome), review severity (cards), gutter bars (editor)
  for (const k of SEMANTIC) on(k, 3, ['bg-0', 'bg-chrome', 'bg-1'], 'semantic');
  // diff and selection backgrounds keep code readable
  on('code-fg', 4.5, [addLine, delLine,
    (x) => blend(x.ok, 0.28, x['bg-0']), (x) => blend(x.danger, 0.28, x['bg-0']),
    (x) => blend(x['sel-tint'], 0.32, x['bg-0'])], 'diff');
  return rules;
}

/** Human-readable failures of one theme (empty when it passes). */
export function contrastProblems(v, group) {
  const out = [];
  for (const r of contrastRules(v)) {
    if (group && r.group !== group) continue;
    const got = r.min(v);
    if (!(got >= r.need)) out.push(`${r.fg} (${r.group}): ${got.toFixed(2)} < ${r.need}`);
  }
  return out;
}
