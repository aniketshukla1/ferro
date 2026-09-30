// CSV/TSV table view: RFC 4180 parsing and delimiter detection (features/csv.js).
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { parseCsv, detectDelimiter } from '../../src/features/csv.js';

test('quoted fields keep delimiters, doubled quotes and newlines', () => {
  const { rows, lines } = parseCsv('name,note\r\n"Smith, J","said ""hi"""\n"multi\nline",x\nlast,row', ',');
  assert.deepEqual(rows, [['name', 'note'], ['Smith, J', 'said "hi"'], ['multi\nline', 'x'], ['last', 'row']]);
  // Row 3 starts on source line 3; its quoted newline pushes row 4 to line 5.
  assert.deepEqual(lines, [1, 2, 3, 5]);
});

test('empty fields and a trailing newline', () => {
  assert.deepEqual(parseCsv('a,,c\n,,\n', ',').rows, [['a', '', 'c'], ['', '', '']]);
});

test('delimiter detection prefers the consistent separator', () => {
  assert.equal(detectDelimiter('a;b;c\n1;2;3\n4;5;6', 'x.csv'), ';');
  assert.equal(detectDelimiter('a,b\n"1,5",2\n3,4', 'x.csv'), ',');
  assert.equal(detectDelimiter('a|b|c\n1|2|3', 'data.txt'), '|');
  assert.equal(detectDelimiter('a,b\n1,2', 'x.tsv'), '\t');
});
