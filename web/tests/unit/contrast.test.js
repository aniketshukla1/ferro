// Theme contrast checks (WCAG 2.1, FRONTEND.md § 5.2): body, muted and code text ≥ 4.5:1; faint text,
// accent and syntax colors ≥ 3:1; semantic state colors and code on diff / selection backgrounds stay
// legible. Covers the boot themes (themes.css) and the lazily loaded pack (themes-extra.css).
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { readThemes, contrastProblems } from './contrast-rules.js';
import { THEME_NOTES } from '../../src/features/theme-notes.js';

const WEB = join(dirname(fileURLToPath(import.meta.url)), '../..');
const T = readThemes(join(WEB, 'styles/themes.css'), join(WEB, 'styles/themes-extra.css'));
const BASE = ['graphite', 'porcelain', 'carbon'];

// The registry in features/themes.js (a browser module, so read as text): list(type, pack, 'ids').
const registry = [...readFileSync(join(WEB, 'src/features/themes.js'), 'utf8').matchAll(/list\('(dark|light)', (true|false), '([\w -]+)'\)/g)]
  .flatMap((m) => m[3].split(' ').map((id) => ({ id, type: m[1], pack: m[2] === 'true' })));

test('16 themes, all registered, one token set', () => {
  assert.equal(Object.keys(T).length, 16);
  assert.deepEqual(registry.slice(0, 3).map((t) => `${t.id}:${t.type}`), ['graphite:dark', 'porcelain:light', 'carbon:dark']);
  for (const t of registry.filter((r) => r.pack)) assert.equal(T[t.id]['bg-0'] < '#808080', t.type === 'dark', `${t.id} is ${t.type}`);
  assert.deepEqual(registry.map((t) => t.id).sort(), Object.keys(T).sort());
  assert.deepEqual(Object.keys(THEME_NOTES).sort(), Object.keys(T).sort());
  const keys = Object.keys(T.graphite).sort();
  for (const [name, v] of Object.entries(T)) assert.deepEqual(Object.keys(v).sort(), keys, `${name} token set`);
});

test('boot CSS holds only graphite / porcelain / carbon; the rest are pack themes', () => {
  const boot = Object.keys(readThemes(join(WEB, 'styles/themes.css'))).sort();
  assert.deepEqual(boot, [...BASE].sort());
  assert.deepEqual(registry.filter((t) => !t.pack).map((t) => t.id).sort(), [...BASE].sort());
});

test('boot-theme.js knows every theme and which ones need the pack', () => {
  const src = readFileSync(join(WEB, 'src/boot-theme.js'), 'utf8');
  const list = (name) => JSON.parse(src.match(new RegExp(`var ${name} = (\\[[^\\]]*\\])`))[1].replace(/'/g, '"'));
  assert.deepEqual(list('base').sort(), registry.filter((t) => !t.pack).map((t) => t.id).sort());
  assert.deepEqual(list('pack').sort(), registry.filter((t) => t.pack).map((t) => t.id).sort());
});

test('light pack themes fall back to porcelain until the pack loads', () => {
  const css = readFileSync(join(WEB, 'styles/themes.css'), 'utf8');
  const sel = css.match(/((?:\[data-theme="[\w-]+"\],\s*)*)\[data-theme="porcelain"\]\s*\{/)[1];
  const fallback = [...sel.matchAll(/"([\w-]+)"/g)].map((m) => m[1]).sort();
  assert.deepEqual(fallback, registry.filter((t) => t.pack && t.type === 'light').map((t) => t.id).sort());
});

for (const [name, v] of Object.entries(T)) {
  test(`${name}: text contrast ≥ 4.5 (faint ≥ 3) on every surface`, () => assert.deepEqual(contrastProblems(v, 'text'), []));
  test(`${name}: accent ≥ 3:1 on the editor`, () => assert.deepEqual(contrastProblems(v, 'accent'), []));
  test(`${name}: every syntax color ≥ 3:1 on the editor and on diff lines`, () => assert.deepEqual(contrastProblems(v, 'syntax'), []));
  test(`${name}: git / review / gutter state colors ≥ 3:1 on editor, chrome and cards`, () => assert.deepEqual(contrastProblems(v, 'semantic'), []));
  test(`${name}: code ≥ 4.5 on diff and selection backgrounds`, () => assert.deepEqual(contrastProblems(v, 'diff'), []));
}
