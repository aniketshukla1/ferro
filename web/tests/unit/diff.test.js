// Diff view and word-wrap logic (FRONTEND.md § 6.9, § 6.12): pure helpers, no DOM.
import { test } from 'node:test';
import assert from 'node:assert/strict';

const { diffKind, imageSides, hunkGap, expandHunk } = await import('../../src/features/diff.js');
const { heightModel } = await import('../../src/core/heights.js');
const { lineCols } = await import('../../src/features/wrap.js');

test('text diffs render as hunks even though every file carries blob ids', () => {
  // The server copies git's `index a..b` line into oldBlob/newBlob for every modified file.
  const text = { path: 'a.json', status: 'M', binary: false, tooLarge: false, oldBlob: 'b1e56dd', newBlob: 'c2f67ee', hunks: [{}] };
  assert.equal(diffKind('a.json', text), 'hunks');
  assert.equal(diffKind('logo.png', { ...text, binary: true }), 'image');
  assert.equal(diffKind('data.bin', { ...text, binary: true }), 'binary');
  assert.equal(diffKind('big.log', { ...text, tooLarge: true, hunks: [] }), 'too-large');
  // SVG is text: the image stage and its source hunks.
  assert.equal(diffKind('icon.svg', text), 'svg');
});

test('image sides follow the change: added has no before, deleted no after, renames read the old path', () => {
  const both = imageSides({ path: 'n.png', oldPath: 'o.png', status: 'R', oldBlob: 'a1', newBlob: 'b2' }, { baseRev: 'abc123' });
  assert.equal(both.before, 'api/v1/git/blob/raw?rev=abc123&path=o.png');
  assert.equal(both.after, 'api/v1/git/blob/raw?rev=worktree&path=n.png');
  assert.equal(imageSides({ path: 'n.png', status: 'A', newBlob: 'b2' }).before, null);
  assert.equal(imageSides({ path: 'n.png', status: '?', newBlob: 'b2' }).before, null);
  assert.equal(imageSides({ path: 'n.png', status: 'D', oldBlob: 'a1' }).after, null);
});

test('hunk expanders grow into the gap on the right side and renumber both sides', () => {
  // Hunk 0 added two lines, so below it the new side runs 2 ahead of the old side.
  const hunks = [
    { oldStart: 10, oldLines: 3, newStart: 10, newLines: 5, rows: [], header: '@@ -10,3 +10,5 @@', section: 'fn a()' },
    { oldStart: 40, oldLines: 2, newStart: 42, newLines: 2, rows: [], header: '@@ -40,2 +42,2 @@' },
  ];
  assert.deepEqual(hunkGap(hunks, 0, 'up'), [1, 9]);
  assert.deepEqual(hunkGap(hunks, 0, 'down'), [15, 41]);
  assert.deepEqual(hunkGap(hunks, 1, 'up'), [15, 41]);
  assert.deepEqual(hunkGap(hunks, 1, 'down'), [44, Infinity]);

  const line = (n) => ({ n, html: `<span>${n}</span>` });
  expandHunk(hunks[1], 'up', [line(40), line(41)]);
  assert.deepEqual(hunks[1].rows.map((r) => [r.o, r.n]), [[38, 40], [39, 41]]);
  assert.equal(hunks[1].header, '@@ -38,4 +40,4 @@');
  expandHunk(hunks[0], 'down', [line(15), line(16)]);
  assert.deepEqual(hunks[0].rows.map((r) => [r.o, r.n]), [[13, 15], [14, 16]]);
  assert.equal(hunks[0].header, '@@ -10,5 +10,7 @@ fn a()');
  // The gap between the two hunks shrank on both ends; once they touch, there is nothing to expand.
  assert.deepEqual(hunkGap(hunks, 0, 'down'), [17, 39]);
  expandHunk(hunks[0], 'down', Array.from({ length: 23 }, (_, i) => line(17 + i)));
  assert.equal(hunkGap(hunks, 0, 'down'), null);
  assert.equal(hunkGap(hunks, 1, 'up'), null);
});

test('height model: prefix sums, row lookup at any y', () => {
  const heights = [20, 60, 20, 40];
  const m = heightModel((i) => heights[i]);
  m.build(heights.length);
  assert.equal(m.total, 140);
  assert.deepEqual([0, 1, 2, 3].map(m.top), [0, 20, 80, 100]);
  assert.equal(m.h(1), 60);
  assert.deepEqual([-5, 0, 19, 20, 79, 80, 139, 500].map((y) => m.at(y)), [0, 0, 0, 1, 1, 2, 3, 3]);
  m.build(0);
  assert.equal(m.at(10), 0);
  assert.equal(m.total, 0);
});

test('wrap columns: tabs to stops, wide characters count two, markup and entities are one each', () => {
  assert.equal(lineCols({ text: 'abc' }), 3);
  assert.equal(lineCols({ text: 'a\tb' }, 4), 5);
  assert.equal(lineCols({ text: '日本' }), 4);
  assert.equal(lineCols({ html: '<span class="t-k">let</span> x = &lt;y&gt;;' }), 12); // `let x = <y>;`
  const cut = { text: 'x'.repeat(10), cut: 500 };
  assert.equal(lineCols(cut), 50); // the "Show full line" button takes room too
});
