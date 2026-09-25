// Boot graph guard (FRONTEND.md § 3.1, § 9): the static import closure of main.js must exclude the
// on-demand features, match next.html's modulepreload list exactly, and stay within the byte budget.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync, statSync } from 'node:fs';
import { join, dirname, relative, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const WEB = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
const SRC = join(WEB, 'src');
const BUDGET_BYTES = 150 * 1024; // uncompressed, comments included (no build step)
const ON_DEMAND = ['features/palette.js', 'features/panels.js', 'features/find.js', 'features/settings.js', 'features/chrome.js', 'features/compat.js', 'features/markdown.js', 'core/match.js'];

function staticImports(file) {
  const text = readFileSync(file, 'utf8');
  const out = [];
  for (const m of text.matchAll(/^\s*(?:import|export)\s(?:[^'"()]*?\sfrom\s*)?['"](\.{1,2}\/[^'"]+)['"]/gm)) out.push(resolve(dirname(file), m[1]));
  return out;
}

function closure(entry) {
  const seen = new Set();
  const stack = [entry];
  while (stack.length) {
    const f = stack.pop();
    if (seen.has(f)) continue;
    seen.add(f);
    stack.push(...staticImports(f));
  }
  return [...seen].map((f) => relative(SRC, f)).sort();
}

const boot = closure(join(SRC, 'main.js'));

test('on-demand features stay out of the boot graph', () => {
  assert.deepEqual(boot.filter((f) => ON_DEMAND.includes(f) || f.startsWith('mock/')), []);
});

test('next.html preloads exactly the boot graph', () => {
  const html = readFileSync(join(WEB, 'next.html'), 'utf8');
  const preload = [...html.matchAll(/<link rel="modulepreload" href="src\/([^"]+)">/g)].map((m) => m[1]).sort();
  assert.deepEqual(preload, boot);
});

test(`boot graph stays under ${BUDGET_BYTES / 1024} KB`, () => {
  const bytes = boot.reduce((n, f) => n + statSync(join(SRC, f)).size, 0);
  assert.ok(bytes <= BUDGET_BYTES, `boot graph is ${(bytes / 1024).toFixed(1)} KB`);
});
