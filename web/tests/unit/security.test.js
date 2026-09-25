// Security invariants for web/src (FRONTEND.md § 4). Run: node --test web/tests/unit/*.test.js
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync, readdirSync, statSync } from 'node:fs';
import { join, relative, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

const SRC = join(dirname(fileURLToPath(import.meta.url)), '../../src');

function files(dir) {
  return readdirSync(dir).flatMap((name) => {
    const p = join(dir, name);
    return statSync(p).isDirectory() ? files(p) : p.endsWith('.js') ? [p] : [];
  });
}

const all = files(SRC).map((p) => ({ rel: relative(SRC, p), text: readFileSync(p, 'utf8') }));

test('the only HTML sink is setTrustedHTML in core/dom.js', () => {
  const offenders = [];
  for (const { rel, text } of all) {
    if (rel === 'core/dom.js') continue;
    const code = text.replace(/\/\/.*$/gm, '');
    if (/\.(innerHTML|outerHTML)\s*=|insertAdjacentHTML\s*\(|document\.write\s*\(/.test(code)) offenders.push(rel);
  }
  assert.deepEqual(offenders, [], `raw HTML sinks outside core/dom.js: ${offenders.join(', ')}`);
});

test('setTrustedHTML is only called with the audited kinds', () => {
  const bad = [];
  for (const { rel, text } of all) {
    for (const m of text.matchAll(/setTrustedHTML\([^,]+,[^,]+,\s*'([^']+)'\)/g)) {
      if (!['hl', 'markdown'].includes(m[1])) bad.push(`${rel}: ${m[1]}`);
    }
  }
  assert.deepEqual(bad, []);
});

test('no inline style attributes or eval (strict CSP)', () => {
  const offenders = [];
  for (const { rel, text } of all) {
    if (rel.startsWith('mock/')) continue;
    if (/setAttribute\(\s*['"]style['"]/.test(text) || /\bstyle="/.test(text) || /\beval\(|new Function\(/.test(text)) offenders.push(rel);
  }
  assert.deepEqual(offenders, []);
});

test('external links always carry rel=noopener noreferrer', () => {
  const offenders = [];
  for (const { rel, text } of all) {
    for (const m of text.matchAll(/target:\s*'_blank'[^}]*\}/g)) {
      if (!/rel:\s*'noopener noreferrer'/.test(m[0])) offenders.push(rel);
    }
  }
  assert.deepEqual(offenders, []);
});
