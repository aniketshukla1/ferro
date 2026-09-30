// Inline edit's line diff (FRONTEND.md § 6.25): what the Changes panel shows.
import { test } from 'node:test';
import assert from 'node:assert/strict';

const { lineDiff } = await import('../../src/features/inline-edit.js');
const kinds = (rows) => rows.map((r) => `${{ ctx: ' ', add: '+', del: '-' }[r.t]}${r.text}`);

test('keeps common head and tail, marks the changed middle', () => {
  assert.deepEqual(kinds(lineDiff(['a', 'b', 'c'], ['a', 'B', 'c'])), [' a', '-b', '+B', ' c']);
  assert.deepEqual(kinds(lineDiff(['a', 'c'], ['a', 'b', 'c'])), [' a', '+b', ' c']);
  assert.deepEqual(kinds(lineDiff(['a', 'b', 'c'], ['a'])), [' a', '-b', '-c']);
});

test('finds the lines both sides keep (LCS), not just head and tail', () => {
  assert.deepEqual(kinds(lineDiff(['x', 'keep', 'y'], ['p', 'keep', 'q'])), ['-x', '+p', ' keep', '-y', '+q']);
});

test('empty sides and a huge middle', () => {
  assert.deepEqual(lineDiff([], []), []);
  assert.deepEqual(kinds(lineDiff([], ['n'])), ['+n']);
  const a = Array.from({ length: 1200 }, (_, i) => `a${i}`);
  const b = Array.from({ length: 1200 }, (_, i) => `b${i}`);
  const rows = lineDiff(a, b);
  assert.equal(rows.filter((r) => r.t === 'del').length, 1200);
  assert.equal(rows.filter((r) => r.t === 'add').length, 1200);
});
