// Pure-logic tests: shortcuts, client fuzzy, mock fuzzy ranking contract, mock highlighter.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { parseKeys, matches } from '../../src/core/keys.js';
import { match, rank } from '../../src/core/match.js';
import { fuzzyScore, fuzzyRank } from '../../src/mock/fuzzy.js';
import { highlight } from '../../src/mock/hl.js';

test('parseKeys: letters and punctuation match on e.code', () => {
  assert.equal(parseKeys('Mod+K').code, 'KeyK');
  assert.equal(parseKeys('Mod+,').code, 'Comma');
  assert.equal(parseKeys('Alt+]').code, 'BracketRight');
  assert.equal(parseKeys('Alt+1').code, 'Digit1');
  assert.equal(parseKeys('F12').key, 'F12');
  assert.equal(parseKeys('Alt+ArrowLeft').key, 'ArrowLeft');
});

test('matches: Option-letter on macOS still matches by code', () => {
  const k = parseKeys('Alt+C');
  // e.key is "ç" on a US Mac keyboard, e.code is still KeyC
  assert.equal(matches({ key: 'ç', code: 'KeyC', altKey: true, shiftKey: false, metaKey: false, ctrlKey: false }, k), true);
  assert.equal(matches({ key: 'c', code: 'KeyC', altKey: false, shiftKey: false, metaKey: false, ctrlKey: false }, k), false);
});

test('matches: "?" ignores the shift needed to type it', () => {
  const k = parseKeys('?');
  assert.equal(matches({ key: '?', code: 'Slash', altKey: false, shiftKey: true, metaKey: false, ctrlKey: false }, k), true);
});

test('match: contiguous beats scattered, positions are exact', () => {
  const a = match('tree', 'features/tree.js');
  assert.deepEqual(a.positions, [9, 10, 11, 12]);
  assert.equal(match('zz', 'features/tree.js'), null);
  const ranked = rank('togsb', ['Toggle Sidebar', 'Go to Symbol', 'Theme'], (x) => x);
  assert.equal(ranked[0].item, 'Toggle Sidebar');
});

const PATHS = [
  'cluster/gce/manifests/kube-scheduler.manifest',
  'pkg/scheduler/scheduler.go',
  'cmd/kube-scheduler/scheduler.go',
  'pkg/scheduler/scheduler_test.go',
  'test/e2e/framework/metrics/scheduler_metrics.go',
  'cmd/kubelet/kubelet.go',
  'pkg/kubelet/kubelet.go',
  'test/e2e/node/kubelet.go',
  'pkg/kubelet/kubelet_pods.go',
].map((path) => ({ path, lower: path.toLowerCase() }));

test('fuzzy contract: exact basename first, abbreviations find the real module', () => {
  const top = fuzzyRank('kubelet.go', PATHS, 3).results.map((r) => r.path);
  assert.ok(top[0].endsWith('/kubelet.go'));
  const sch = fuzzyRank('schdlr', PATHS, 3).results.map((r) => r.path);
  assert.ok(sch.includes('pkg/scheduler/scheduler.go'), sch.join(', '));
});

test('fuzzy positions index the path (UTF-16)', () => {
  const r = fuzzyScore('srv', 'crates/ferro-server/src/server.rs');
  assert.ok(r);
  for (const p of r.positions) assert.ok(p >= 0 && p < 'crates/ferro-server/src/server.rs'.length);
});

test('mock highlighter escapes markup and never emits other tags', () => {
  const html = highlight('let x = "<img src=x onerror=alert(1)>";\n// <script>', 'rust').join('\n');
  assert.ok(!html.includes('<img'), html);
  assert.ok(!html.includes('<script'), html);
  assert.ok(!/<(?!\/?span[ >])/.test(html), 'only <span> tags allowed');
});

test('legacy porcelain status parses into the v1 GitStatus shape', async () => {
  const { parsePorcelain } = await import('../../src/features/compat.js');
  const g = parsePorcelain([
    '## release/1.2...origin/release/1.2 [ahead 2, behind 1]',
    'M  staged.rs',
    ' M web/app.js',
    'MM both.ts',
    'R  old name.md -> docs/new.md',
    'UU conflict.txt',
    '?? web/src/',
    '?? "caf\\303\\251 notes.txt"',
    '',
  ].join('\n'));
  assert.equal(g.branch, 'release/1.2');
  assert.equal(g.upstream, 'origin/release/1.2');
  assert.deepEqual([g.ahead, g.behind], [2, 1]);
  assert.deepEqual(g.counts, { staged: 3, unstaged: 2, untracked: 2, conflicted: 1 });
  assert.deepEqual(g.files.find((f) => f.path === 'docs/new.md'), { path: 'docs/new.md', origPath: 'old name.md', index: 'R', worktree: null, untracked: false, conflicted: false });
  assert.equal(g.files.find((f) => f.path === 'web/src').dir, true);
  assert.ok(g.files.some((f) => f.path === 'café notes.txt'));
  assert.equal(parsePorcelain('## HEAD (no branch)\n').detached, true);
});

test('find in file: smart case, whole word, regex, CRLF and UTF-16 ranges', async () => {
  const { compileQuery, findInText } = await import('../../src/features/find.js');
  const text = 'Foo foo\r\nthis is it\n😀x = fooBar\n';
  assert.equal(findInText(text, compileQuery('foo')).total, 3); // smart: lowercase query ignores case
  assert.equal(findInText(text, compileQuery('Foo')).total, 1); // uppercase makes it sensitive
  assert.deepEqual(findInText(text, compileQuery('is', { word: true })).matches, [{ line: 2, ranges: [[5, 7]] }]);
  assert.deepEqual(findInText(text, compileQuery('x', { caseMode: 'sensitive' })).matches, [{ line: 3, ranges: [[2, 3]] }]);
  assert.equal(findInText(text, compileQuery('f[o]+', { mode: 'regex' })).total, 3);
  assert.equal(findInText('a.b a-b', compileQuery('a.b')).total, 1); // literal dot
  assert.throws(() => compileQuery('(', { mode: 'regex' }));
  const capped = findInText('aaaa\naaaa', compileQuery('a'), 5);
  assert.equal(capped.total, 5);
  assert.equal(capped.truncated, true);
  assert.equal(findInText('x', compileQuery('^', { mode: 'regex' })).total, 0); // empty matches are skipped
});

test('joinPath resolves relative markdown links inside the workspace', async () => {
  const { joinPath } = await import('../../src/core/util.js');
  assert.equal(joinPath('docs/spec', 'API.md'), 'docs/spec/API.md');
  assert.equal(joinPath('docs/spec', '../ROADMAP.md'), 'docs/ROADMAP.md');
  assert.equal(joinPath('docs', './a/./b.md'), 'docs/a/b.md');
  assert.equal(joinPath('docs', '../../../etc/passwd'), 'etc/passwd');
  assert.equal(joinPath('', 'README.md'), 'README.md');
});

test('search hits drop indentation and keep the match in view', async () => {
  const { focusHit } = await import('../../src/features/panels.js');
  const a = focusHit('        server::build_router(state)', [[16, 28]]);
  assert.equal(a.text, 'server::build_router(state)');
  assert.deepEqual(a.ranges, [[8, 20]]);
  assert.equal(a.text.slice(...a.ranges[0]), 'build_router');
  const long = `${'x'.repeat(60)} needle`;
  const b = focusHit(long, [[61, 67]]);
  assert.ok(b.text.startsWith('…'));
  assert.equal(b.text.slice(...b.ranges[0]), 'needle');
  assert.deepEqual(focusHit('plain', [[0, 5]]), { text: 'plain', ranges: [[0, 5]] });
});

test('mock markdown renders the sanitized backend shape and escapes hostile input', async () => {
  const { renderMarkdown } = await import('../../src/mock/markdown.js');
  const src = [
    '# Title <script>alert(1)</script>',
    '',
    'See [spec](../API.md#L12), [site](https://example.com) and ![x](https://evil.test/p.png) <img src=x onerror=alert(1)>',
    '',
    '- [x] done `<b>`',
    '',
    '```js',
    'const a = "<svg onload=alert(1)>";',
    '```',
  ].join('\n');
  const r = renderMarkdown(src, { path: 'docs/spec/README.md', rawUrl: (p) => `/raw/${p}` });
  assert.equal(r.externalImages, 1);
  assert.equal(r.headings[0].line, 1);
  assert.ok(r.html.includes('data-path="docs/API.md" data-line="12"'), r.html);
  assert.ok(r.html.includes('data-ext-src="https://evil.test/p.png"') && !/<img[^>]*\ssrc="https:\/\/evil/.test(r.html));
  assert.ok(!/<script|onerror=|onload=|<svg/i.test(r.html.replace(/&lt;[^&]*&gt;/g, '')), r.html);
  const tags = new Set([...r.html.matchAll(/<\/?([a-z0-9]+)/g)].map((m) => m[1]));
  const allowed = new Set(['p', 'h1', 'h2', 'h3', 'h4', 'h5', 'h6', 'a', 'img', 'ul', 'ol', 'li', 'input', 'blockquote', 'pre', 'code', 'table', 'thead', 'tbody', 'tr', 'th', 'td', 'em', 'strong', 'del', 'hr', 'span']);
  assert.deepEqual([...tags].filter((t) => !allowed.has(t)), []);
});

test('mock highlighter: python # comments do not bleed into the next line', () => {
  const [l1, l2] = highlight('x = 1  # comment\ny = "s"\n', 'python');
  assert.ok(l1.includes('t-c'));
  assert.ok(!l2.includes('t-c'), l2);
  assert.ok(l2.includes('t-s'), l2);
});
