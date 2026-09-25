// Theme contrast checks (WCAG 2.1): body, muted and code text ≥ 4.5:1; faint text, accent and syntax colors ≥ 3:1.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

const css = readFileSync(join(dirname(fileURLToPath(import.meta.url)), '../../styles/themes.css'), 'utf8');

function themes() {
  const out = {};
  for (const m of css.matchAll(/\[data-theme="([\w-]+)"\]\s*\{([^}]*)\}/g)) {
    const vars = {};
    for (const v of m[2].matchAll(/--([\w-]+):\s*(#[0-9a-fA-F]{6})\s*;/g)) vars[v[1]] = v[2];
    out[m[1]] = vars;
  }
  return out;
}

function lum(hex) {
  const c = [1, 3, 5].map((i) => parseInt(hex.slice(i, i + 2), 16) / 255).map((x) => (x <= 0.03928 ? x / 12.92 : ((x + 0.055) / 1.055) ** 2.4));
  return 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
}
export function ratio(a, b) {
  const [x, y] = [lum(a), lum(b)].sort((p, q) => q - p);
  return (x + 0.05) / (y + 0.05);
}

const T = themes();

test('every theme is parsed', () => {
  assert.deepEqual(Object.keys(T).sort(), ['carbon', 'graphite', 'porcelain']);
});

for (const [name, v] of Object.entries(T)) {
  test(`${name}: text contrast ≥ 4.5 on every surface`, () => {
    const problems = [];
    for (const bg of ['bg-0', 'bg-chrome', 'bg-3']) {
      for (const fg of ['fg', 'fg-muted']) {
        const r = ratio(v[fg], v[bg]);
        if (r < 4.5) problems.push(`${fg} on ${bg}: ${r.toFixed(2)}`);
      }
      const faint = ratio(v['fg-faint'], v[bg]);
      if (faint < 3) problems.push(`fg-faint on ${bg}: ${faint.toFixed(2)}`);
    }
    const code = ratio(v['code-fg'], v['bg-0']);
    if (code < 4.5) problems.push(`code-fg on bg-0: ${code.toFixed(2)}`);
    assert.deepEqual(problems, []);
  });

  test(`${name}: accent usable as text and syntax colors ≥ 3:1 on the editor`, () => {
    const problems = [];
    const acc = ratio(v.accent, v['bg-0']);
    if (acc < 3) problems.push(`accent on bg-0: ${acc.toFixed(2)}`);
    for (const [k, color] of Object.entries(v)) {
      if (!k.startsWith('syn-')) continue;
      const r = ratio(color, v['bg-0']);
      if (r < 3) problems.push(`${k}: ${r.toFixed(2)}`);
    }
    assert.deepEqual(problems, []);
  });
}
