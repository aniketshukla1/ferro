// Escape-first streaming renderer (FRONTEND.md § 4 rule 7): parseMiniMarkdown never interprets
// HTML, and createAnswerStream swaps to the final backend markdown exactly once.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { parseMiniMarkdown, createAnswerStream } from '../../src/core/minimd.js';

test('parseMiniMarkdown treats a script payload as plain text, never as markup', () => {
  const payload = '<script>alert(document.cookie)</script>';
  const tokens = parseMiniMarkdown(payload);
  assert.deepEqual(tokens, [{ type: 'text', value: payload }]);
  // No token carries anything that looks like it was parsed out of the tag structure.
  assert.ok(!tokens.some((t) => t.type !== 'text' && t.type !== 'br'));
});

test('parseMiniMarkdown only recognizes code/bold/em inline marks, and only as text runs', () => {
  const tokens = parseMiniMarkdown('a `<b>code</b>` and **<img onerror=x>** and *<i>em</i>*');
  const byType = (t) => tokens.filter((tok) => tok.type === t).map((tok) => tok.value);
  assert.deepEqual(byType('code'), ['<b>code</b>']);
  assert.deepEqual(byType('bold'), ['<img onerror=x>']);
  assert.deepEqual(byType('em'), ['<i>em</i>']);
  // The angle brackets inside those marks are carried as literal text values, not consumed as tags.
  for (const t of tokens) assert.equal(typeof t.value, t.type === 'br' ? 'undefined' : 'string');
});

test('parseMiniMarkdown splits lines into "br" tokens without collapsing content', () => {
  const tokens = parseMiniMarkdown('line one\nline two');
  assert.deepEqual(tokens, [
    { type: 'text', value: 'line one' },
    { type: 'br' },
    { type: 'text', value: 'line two' },
  ]);
});

test('createAnswerStream: token() accumulates until final(), which swaps exactly once', () => {
  const stream = createAnswerStream();
  assert.equal(stream.finalized, false);
  assert.equal(stream.token('Hello'), 'Hello');
  assert.equal(stream.token(', world'), 'Hello, world');
  assert.equal(stream.text, 'Hello, world');

  const html = stream.final('<p>Hello, world</p>');
  assert.equal(html, '<p>Hello, world</p>');
  assert.equal(stream.finalized, true);

  // A second final() call is a no-op (idempotent swap: exactly once).
  assert.equal(stream.final('<p>different</p>'), false);
});

test('createAnswerStream: tokens arriving after final() are ignored, not appended', () => {
  const stream = createAnswerStream();
  stream.token('partial answer');
  stream.final('<p>final</p>');
  assert.equal(stream.token(' more'), null);
  assert.equal(stream.text, 'partial answer');
});
