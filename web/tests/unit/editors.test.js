// The VS Code extension's pure helpers (editors/vscode/lib.js): what it hands `ferro open`.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';

const { target, openArgs, quote, NO_SERVER } = createRequire(import.meta.url)('../../../editors/vscode/lib.js');

test('file views get file:line, folder views get the folder', () => {
  const at = { file: '/w/app/src/a.rs', line: 12, folder: '/w/app' };
  assert.equal(target(at), '/w/app/src/a.rs:12');
  assert.equal(target(at, 'diff'), '/w/app/src/a.rs:12');
  assert.equal(target(at, 'checks'), '/w/app');
  assert.equal(target({ folder: '/w/app' }), '/w/app');
  assert.equal(target({ file: '/tmp/x.txt' }, 'changes'), '/tmp/x.txt');
  assert.equal(target({}), null);
});

test('arguments and terminal quoting', () => {
  assert.deepEqual(openArgs('/w/a.rs:3', 'diff', { noServe: true }), ['open', '/w/a.rs:3', '--view', 'diff', '--no-serve']);
  assert.deepEqual(openArgs('/w'), ['open', '/w']);
  assert.equal(quote('/w/app/src/a.rs:12'), '/w/app/src/a.rs:12');
  assert.equal(quote("/w/it's here"), "'/w/it'\\''s here'");
  assert.equal(quote("C:\\w\\it's", true), "'C:\\w\\it''s'");
  assert.equal(NO_SERVER, 3);
});
