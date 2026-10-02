// Boot graph guard (FRONTEND.md § 3.1, § 9): the static import closure of main.js must exclude the
// on-demand features, match index.html's modulepreload list exactly, and stay within the byte budget.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync, statSync, readdirSync } from 'node:fs';
import { join, dirname, relative, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const WEB = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
const SRC = join(WEB, 'src');
const BUDGET_BYTES = 160 * 1024; // uncompressed, comments included (no build step); FRONTEND.md § 9
const ON_DEMAND = ['features/palette.js', 'features/panels.js', 'features/find.js', 'features/settings.js', 'features/chrome.js', 'features/markdown.js', 'features/image.js', 'features/git.js', 'features/diff.js', 'features/review.js', 'features/ai.js', 'features/theme-notes.js', 'features/nav.js', 'features/agent.js', 'features/hud.js', 'features/tips.js', 'features/vim.js', 'features/threads.js', 'features/csv.js', 'features/problems.js', 'features/update.js', 'features/history.js', 'features/explain.js', 'features/checks.js', 'features/memory.js', 'features/repos.js', 'features/intent.js', 'ui/dialog.js', 'core/match.js'];

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

test('index.html preloads exactly the boot graph', () => {
  const html = readFileSync(join(WEB, 'index.html'), 'utf8');
  const preload = [...html.matchAll(/<link rel="modulepreload" href="src\/([^"]+)">/g)].map((m) => m[1]).sort();
  assert.deepEqual(preload, boot);
});

test('F2 close-out: legacy UI is gone, / is the new UI, /next.html forwards', () => {
  const index = readFileSync(join(WEB, 'index.html'), 'utf8');
  assert.match(index, /<script type="module" src="src\/main\.js"><\/script>/);
  for (const f of ['legacy.html', 'legacy/app.js', 'legacy/style.css']) {
    assert.throws(() => statSync(join(WEB, f)), f);
  }
  const next = readFileSync(join(WEB, 'next.html'), 'utf8');
  assert.match(next, /src="src\/next-redirect\.js"/);
  assert.doesNotMatch(next, /src\/main\.js/);
});

test(`boot graph stays under ${BUDGET_BYTES / 1024} KB`, () => {
  const bytes = boot.reduce((n, f) => n + statSync(join(SRC, f)).size, 0);
  assert.ok(bytes <= BUDGET_BYTES, `boot graph is ${(bytes / 1024).toFixed(1)} KB`);
});

// Shipped JS: src/mock/ (the ?mock=1 dev/e2e backend) never loads in a real session.
// Everything outside the boot graph loads on first use, so the total guards bloat, not startup;
// the per-module cap keeps any one on-demand feature from growing unnoticed (FRONTEND.md § 9).
function shippedModules(dir = SRC, out = []) {
  for (const ent of readdirSync(dir, { withFileTypes: true })) {
    const full = join(dir, ent.name);
    if (ent.isDirectory()) { if (full !== join(SRC, 'mock')) shippedModules(full, out); }
    else if (ent.isFile() && ent.name.endsWith('.js')) out.push([relative(SRC, full), statSync(full).size]);
  }
  return out;
}

// 512 KB → 528 KB on 2026-09-29 (the user's call) for inline edit, which loads on first use;
// 528 KB → 560 KB on 2026-10-02 (the user's call), starting with opening repositories.
test('total shipped web/src JS stays under 560 KB', () => {
  const total = shippedModules().reduce((n, [, size]) => n + size, 0);
  assert.ok(total <= 560 * 1024, `total shipped web/src is ${(total / 1024).toFixed(1)} KB`);
});

test('no shipped module is over 64 KB', () => {
  const big = shippedModules().filter(([, size]) => size > 64 * 1024).map(([f, size]) => `${f} ${(size / 1024).toFixed(1)} KB`);
  assert.deepEqual(big, []);
});
