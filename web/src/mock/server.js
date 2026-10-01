// Mock implementation of the API v1 surface the frontend uses today.
// Shapes follow the real API exactly so switching to the real backend is a no-op.
import { showcaseExplain } from './showcase.js';
import { ApiError } from '../core/api.js';
import { storage, sleep, plural } from '../core/util.js';
import { MockRepo, hugeLine, hugeLineHtml } from './repo.js';
import { fuzzyRank } from './fuzzy.js';
import { renderMarkdown } from './markdown.js';
import { highlight, languageFor } from './hl.js';

// Mirrors the B1 schema (GET /settings/schema) minus the legacy UI keys.
const MOCK_SCHEMA = [
  { key: 'files.exclude', section: 'files', title: 'Excluded globs', description: 'Extra ignore globs for the file index.', type: 'string[]', default: [], scopes: ['user', 'workspace'] },
  { key: 'search.exclude', section: 'search', title: 'Search excludes', description: 'Globs skipped by content search.', type: 'string[]', default: ['**/vendor/**'], scopes: ['user', 'workspace'] },
  { key: 'search.maxFileBytes', section: 'search', title: 'Max file size', description: 'Skip files larger than this many bytes.', type: 'int', default: 8388608, min: 1024, scopes: ['user', 'workspace'] },
  { key: 'review.defaultEvent', section: 'review', title: 'Default review event', description: 'Preselected when submitting a review.', type: 'enum', default: 'COMMENT', enum: ['COMMENT', 'APPROVE', 'REQUEST_CHANGES'], scopes: ['user', 'workspace'] },
  { key: 'ai.provider', section: 'ai', title: 'Provider', description: 'LLM provider selection.', type: 'enum', default: 'auto', enum: ['auto', 'anthropic', 'openai', 'gemini', 'ollama', 'openai-compatible', 'off'], scopes: ['user', 'workspace'] },
  { key: 'ai.model', section: 'ai', title: 'Model', description: 'Model override (empty = provider default).', type: 'string', default: '', scopes: ['user', 'workspace'] },
  { key: 'ai.redactSecrets', section: 'ai', title: 'Redact secrets', description: 'Mask likely secrets before sending context.', type: 'bool', default: true, scopes: ['user', 'workspace'] },
  { key: 'update.check', section: 'updates', title: 'Check for updates', description: 'Look for new releases at startup.', type: 'bool', default: false, scopes: ['user'] },
];

const FEATURES = [
  'v1', 'events', 'settings', 'session', 'tree', 'file', 'hl.classes', 'hl.exact', 'markdown.v2', 'outline',
  'jobs', 'workspace.open', 'metrics', 'fuzzy.v2', 'search.v2', 'search.regex', 'file.find', 'paths.resolve', 'git.status.v2', 'git.changes', 'git.diff', 'git.blob.lines', 'git.gutter', 'git.stage', 'git.unstage', 'git.discard', 'git.commit', 'git.push', 'git.pull', 'git.log', 'auth.logout',
  'pr.open', 'review.drafts', 'review.viewed', 'review.rounds', 'review.submit', 'pr.threads', 'pr.conversation', 'markdown.render',
  'ai', 'ai.ask', 'ai.review', 'ai.commit',
  'symbols', 'nav', 'harness', 'git.hunk', 'harness.threads', 'lsp.diagnostics', 'update.auto', 'git.history', 'ai.explain', 'checks.breaking', 'checks.tests', 'checks.security', 'checks.coverage', 'memory',
  'file.edit', 'ai.edit', 'memory.learn',
];
// Language-server diagnostics the mock reports once a file is opened (API.md § 4.9).
const MOCK_DIAGNOSTICS = {
  'crates/ferro-core/src/fuzzy.rs': [
    { line: 12, col: 9, endLine: 12, endCol: 17, severity: 'error', message: 'mismatched types\nexpected `usize`, found `u32`', source: 'rustc', code: 'E0308' },
    { line: 30, col: 5, endLine: 30, endCol: 12, severity: 'warning', message: 'unused variable: `scratch`', source: 'rustc', code: 'unused_variables' },
  ],
  'crates/ferro-core/src/text.rs': [
    { line: 4, col: 1, endLine: 4, endCol: 20, severity: 'warning', message: 'function `legacy_width` is never used', source: 'rustc', code: 'dead_code' },
  ],
};
const lspOpened = new Set();
// Team review memory (API.md § 17): rules, and one suggestion from two earlier dismissals.
const mockMemory = { team: [], personal: [], dismissed: [], learnedAt: null };
// What "Learn from PRs" finds on the sample repository: two conventions, each with the review
// comments behind it.
const ev = (pr, author, excerpt) => ({ pr, author, excerpt, url: `https://github.com/aniketshukla1/ferro/pull/${pr}` });
const MOCK_LEARNED = [
  { text: 'Add a regression test with every bug fix', category: 'tests', evidence: [ev(412, 'riya', 'Please add a regression test that fails without this fix.'), ev(398, 'sam', 'Can we get a test that reproduces the original bug?'), ev(377, 'riya', 'Needs a test before we merge, this regressed once already.')] },
  { text: 'Return errors with context instead of unwrap in library code', category: 'errors', evidence: [ev(405, 'sam', 'Avoid unwrap here, return the error with the path it failed on.'), ev(381, 'ann', 'This unwrap will panic on a bad config; map it to an error instead.')] },
];
const learnedSuggestions = () => (mockMemory.learnedAt ? MOCK_LEARNED : []).map((l) => {
  const key = `merged|${l.text.toLowerCase()}`;
  const prs = new Set(l.evidence.map((e) => e.pr)).size;
  return { key, source: 'merged', count: l.evidence.length, examples: l.evidence.map((e) => e.excerpt), evidence: l.evidence,
    why: `Asked for in ${l.evidence.length} review comments across ${prs} merged pull requests`,
    rule: { id: '', kind: 'convention', appliesTo: 'ai', category: l.category, text: l.text, paths: [], reason: `Reviewers asked for this in ${prs} merged pull requests`, author: '', createdAt: '' } };
}).filter((x) => !mockMemory.dismissed.includes(x.key) && ![...mockMemory.team, ...mockMemory.personal].some((r) => r.text?.toLowerCase() === x.rule.text.toLowerCase()));
const MOCK_SUGGESTION = { key: 'dismiss|ai|style|magic number', count: 2, why: 'Dismissed 2 times in crates/ferro-core/src/**', examples: ['crates/ferro-core/src/fuzzy.rs', 'crates/ferro-core/src/scan.rs'],
  rule: { id: '', kind: 'ignore', appliesTo: 'ai', category: 'style', title: 'Magic number', paths: ['crates/ferro-core/src/**'], reason: '', author: '', createdAt: '' } };
function memoryMatches(r, f) {
  if (r.kind !== 'ignore' || (r.appliesTo !== 'any' && r.appliesTo !== 'security') || !r.rule) return false;
  const ok = r.rule.endsWith('*') ? f.rule.startsWith(r.rule.slice(0, -1)) : f.rule === r.rule;
  const inPaths = !r.paths?.length || r.paths.some((g) => (g.endsWith('/**') ? f.path.startsWith(g.slice(0, -2)) : f.path === g));
  return ok && inPaths;
}
function withMemory(findings) {
  return findings.map((f) => {
    const r = [...mockMemory.team, ...mockMemory.personal].find((x) => memoryMatches(x, f));
    return r ? { ...f, suppressedBy: { id: r.id, reason: r.reason, scope: mockMemory.team.includes(r) ? 'team' : 'personal' } } : f;
  });
}
const mockUpdate = { current: '0.2.0-dev', latest: null, state: 'idle', error: null, checkedAt: null, configured: true, canInstall: true };
const LIMITS = { maxWindowLines: 1000, maxCols: 4000, maxRawBytes: 33554432, maxMarkdownBytes: 4194304, maxSearchFiles: 1000, maxDiffRows: 20000 };

// ---------- AI (F4 / B5) ----------
const MOCK_FINDING_SOURCE = [
  { path: 'crates/ferro-core/src/search.rs', line: 42, side: 'RIGHT', severity: 'high', category: 'bug', title: 'Off-by-one on the trigram fallback', body: 'When `query.len() < 3` this falls back to a full scan, but the loop bound is `<=` so the last file is scanned twice.', suggestion: 'if idx <= entries.len() {', confidence: 0.86 },
  { path: 'crates/ferro-core/src/search.rs', line: 88, side: 'RIGHT', severity: 'medium', category: 'performance', title: 'Unbounded allocation per match', body: 'Each hit clones the full line into a new `String`. For a 400k-line file with many matches this dominates the request.', confidence: 0.64 },
  { path: 'crates/ferro-core/src/search.rs', line: 15, side: 'RIGHT', severity: 'nit', category: 'style', title: 'Prefer `is_empty()` over `len() == 0`', body: 'Idiomatic Rust and avoids a redundant length computation on some collections.', suggestion: 'if query.is_empty() {', confidence: 0.95 },
  { path: 'crates/ferro-cli/src/server.rs', line: 120, side: 'RIGHT', severity: 'high', category: 'security', title: 'Path is not canonicalized before the existence check', body: 'A workspace-relative path with `..` segments can escape the workspace root before this check runs, so the later read can serve files outside it.', confidence: 0.78 },
  { path: 'crates/ferro-cli/src/server.rs', line: 205, side: 'RIGHT', severity: 'low', category: 'tests', title: 'No test for the empty-body case', body: 'The handler returns `400` for an empty request body, but no test exercises that branch.', confidence: 0.55 },
];

const FOCUS_OF_CATEGORY = { bug: 'bugs', performance: 'performance', style: 'maintainability', security: 'security', tests: 'tests' };

function buildMockFindings(body = {}) {
  const focus = body.focus?.length ? new Set(body.focus) : null;
  return MOCK_FINDING_SOURCE
    .filter((f) => !focus || focus.has(FOCUS_OF_CATEGORY[f.category] || f.category))
    .map((f, i) => ({
      id: `find_${i}`,
      path: f.path,
      line: f.line,
      side: f.side,
      severity: f.severity,
      category: f.category,
      title: f.title,
      body: f.body,
      bodyHtml: renderMarkdown(f.body, { path: f.path, rawUrl: (p) => new URL(`../${p}`, document.baseURI).toString() }).html,
      suggestion: f.suggestion,
      confidence: f.confidence,
    }));
}

function buildMockAskAnswer(question, path) {
  const q = String(question || '').trim();
  const parts = [`You asked: "${q}". `];
  if (path) parts.push(`Looking at \`${path}\`, `, 'the relevant logic starts near the top of the file. ');
  parts.push('In short: this repository indexes files with a trigram engine and falls back to a ', 'linear scan for short queries (under three characters). ', 'The fallback is the usual place to look first when a search feels slow.');
  return parts;
}

export function createMockServer(opts) {
  const repo = new MockRepo({ big: opts.big });
  // Code navigation fixtures: symbols from the outline of every web/src JS module (not mock/).
  const NAV_FILES = repo.entries.map((e) => e.path).filter((p) => /^web\/src\/(core|features|ui)\/[\w-]+\.js$/.test(p));
  let symIndex = null;
  async function symbolIndex() {
    if (!symIndex) {
      symIndex = (async () => {
        const out = [];
        for (const path of NAV_FILES) {
          const doc = await repo.doc(path);
          if (doc && !doc.huge) for (const s of outline(doc)) out.push({ path, name: s.name, kind: s.kind, line: s.line });
        }
        return out;
      })();
    }
    return symIndex;
  }
  async function wordAt(q) {
    const doc = await repo.doc(need(q, 'path'));
    const text = doc?.lines?.[Number(q.line) - 1];
    if (text == null) throw new ApiError(404, 'not_found', `cannot read: ${q.path}`);
    const at = Math.max(0, Number(q.col || 1) - 1);
    const isW = (c) => /[\w$]/.test(c || '');
    if (!isW(text[at])) throw new ApiError(404, 'not_found', 'no definition here');
    let a = at;
    let b = at;
    while (isW(text[a - 1])) a--;
    while (isW(text[b + 1])) b++;
    return { word: text.slice(a, b + 1) };
  }
  const started = Date.now();
  let bootedAt = started; // metrics uptime; a mock restart resets it
  const state = {
    index: { state: 'indexing', files: 0, ms: 0, generation: 1, searchIndex: 'off' },
    settings: storage.get('ferro.mock.settings', {}),
    session: storage.get('ferro.mock.session', null),
  };
  const events = new Set();
  const emit = (type, data) => events.forEach((fn) => fn(type, data));

  // -------- Harness edits (F4b / B7): opt-in, jobs, snapshot diffs, per-hunk revert --------
  const harness = {
    selected: null,
    model: '',
    harnesses: [
      { id: 'claude', label: 'Claude Code', installed: true, models: [], defaultModel: '' },
      { id: 'codex', label: 'Codex', installed: true, models: [], defaultModel: '' },
      { id: 'aider', label: 'Aider', installed: false, models: [], defaultModel: '' },
    ],
  };
  const harnessState = () => ({ selected: harness.selected, pinned: !!harness.selected, model: harness.model, harnesses: harness.harnesses });
  const agentRunning = new Map(); // jobId -> { path, start, end }
  const jobCancel = new Map(); // jobId -> cancel()
  const threads = new Map(); // agent threads (API.md § 10.7)
  const agentEdits = new Map(); // snapshot base -> { jobId, path, lang, hunks, reverted:Set }
  const liveHunks = (e) => e.hunks.filter((hk) => !e.reverted.has(hk.id));
  const hlRow = (t, o, n, text, lang) => ({ t, o, n, text, html: highlight(text, lang)[0] ?? '' });
  function agentHunks(doc, start, end, instruction) {
    const L = (n) => doc.lines[n - 1] ?? '';
    const rows = [];
    if (start > 1) rows.push(hlRow('ctx', start - 1, start - 1, L(start - 1), doc.lang));
    rows.push(hlRow('del', start, null, L(start), doc.lang));
    rows.push(hlRow('add', null, start, `${L(start).replace(/\s+$/, '')} // edited: ${instruction.slice(0, 40)}`, doc.lang));
    if (start < doc.total) rows.push(hlRow('ctx', start + 1, start + 1, L(start + 1), doc.lang));
    const s1 = start > 1 ? start - 1 : start;
    const n1 = rows.filter((r) => r.t !== 'add').length;
    const hunks = [{ id: 'hunk_1', header: `@@ -${s1},${n1} +${s1},${n1} @@`, section: '', oldStart: s1, oldLines: n1, newStart: s1, newLines: n1, rows }];
    const at = Math.min(doc.total, end + 4);
    if (at > start + 2) {
      hunks.push({
        id: 'hunk_2', header: `@@ -${at},1 +${at},2 @@`, section: '', oldStart: at, oldLines: 1, newStart: at, newLines: 2,
        rows: [hlRow('ctx', at, at, L(at), doc.lang), hlRow('add', null, at + 1, `// agent note: ${instruction.slice(0, 60)}`, doc.lang)],
      });
    }
    return hunks;
  }
  function snapChanges(base) {
    const e = agentEdits.get(base);
    const live = liveHunks(e);
    const additions = live.reduce((n, hk) => n + hk.rows.filter((r) => r.t === 'add').length, 0);
    const deletions = live.reduce((n, hk) => n + hk.rows.filter((r) => r.t === 'del').length, 0);
    const files = live.length ? [{ path: e.path, status: 'M', additions, deletions, binary: false }] : [];
    return { base, baseSha: base, target: 'worktree', targetSha: null, stats: { files: files.length, additions, deletions }, files };
  }

  // Simulated background index walk.
  setTimeout(() => {
    state.index = { state: 'ready', files: repo.entries.length, ms: opts.big ? 212 : 17, generation: 2, searchIndex: 'off' };
    emit('index', state.index);
  }, 350);

  const meta = () => ({
    api: 1, specVersion: '1.0', version: '0.2.0-dev', host: 'cli', mode: 'workspace', readOnly: false,
    workspace: { root: '/Users/you/ferro', name: 'ferro', key: 'a1b2c3d4e5f60718', git: true, branch: 'main', headSha: '9377e11a4c1f0f5d2d8b0a6f1e2d3c4b5a697886' },
    index: state.index,
    features: FEATURES,
    auth: { remember: true, rememberDays: 30 },
    limits: LIMITS,
    pr: repo.pr,
  });

  const need = (q, k) => {
    if (q[k] === undefined || q[k] === '') throw new ApiError(400, 'bad_request', `missing ${k}`, { field: k });
    return q[k];
  };

  const routes = {
    'GET meta': () => meta(),
    'GET settings': () => ({ scope: 'effective', values: state.settings, defaults: {} }),
    'PUT settings': (q, b) => {
      for (const [k, v] of Object.entries(b.values || {})) {
        if (v === null) delete state.settings[k];
        else state.settings[k] = v;
      }
      storage.set('ferro.mock.settings', state.settings);
      emit('settings', { values: state.settings });
      return { scope: 'effective', values: state.settings, defaults: {} };
    },
    'GET session': () => ({ data: state.session, updatedAt: null }),
    'PUT session': (q, b) => {
      state.session = b.data;
      storage.set('ferro.mock.session', b.data);
      return { updatedAt: new Date().toISOString() };
    },
    'GET tree': (q) => {
      const entries = repo.children(q.dir || '');
      if (!entries) throw new ApiError(404, 'not_found', 'no such directory');
      return { dir: q.dir || '', generation: state.index.generation, entries };
    },
    'GET file': async (q) => {
      const path = need(q, 'path');
      const m = repo.meta(path);
      if (!m) throw new ApiError(404, 'not_found', `no such file: ${path}`);
      const base = {
        path, size: m.e.size, mtimeMs: started, kind: m.kind, language: repo.languageName(path),
        markdown: /\.(md|markdown)$/i.test(path), eol: 'lf', encoding: 'utf-8', git: repo.git.get(path) || null, tooLarge: false,
      };
      if (m.kind !== 'text') return { ...base, lines: 0, mime: m.kind === 'image' ? 'image/*' : undefined };
      const doc = await repo.doc(path);
      return { ...base, lines: doc?.total ?? 0, eol: doc?.eol || 'lf' };
    },
    'GET file/lines': async (q) => {
      const path = need(q, 'path');
      const from = Math.max(1, parseInt(q.from || '1', 10));
      const count = Math.min(LIMITS.maxWindowLines, Math.max(1, parseInt(q.count || '500', 10)));
      const doc = await repo.doc(path);
      if (!doc) throw new ApiError(404, 'not_found', `no such file: ${path}`);
      const lines = [];
      const end = Math.min(doc.total, from + count - 1);
      if (doc.huge) {
        for (let n = from; n <= end; n++) lines.push({ n, html: hugeLineHtml(n - 1) });
      } else {
        const plain = q.hl === '0' || q.hl === 0;
        const html = plain ? [] : repo.highlighted(doc);
        for (let n = from; n <= end; n++) {
          const raw = doc.lines[n - 1] ?? '';
          const line = plain ? { n, text: raw.slice(0, LIMITS.maxCols) } : { n, html: html[n - 1] ?? '' };
          if (raw.length > LIMITS.maxCols) line.cut = raw.length;
          lines.push(line);
        }
      }
      return { path, from, total: doc.total, mtimeMs: started, exact: true, language: repo.languageName(path), lines };
    },
    // Inline edits (API.md § 4.10) change the in-memory document; the fs event reloads views.
    'POST file/edit': async (q, b) => {
      const path = need(b, 'path');
      const doc = await repo.doc(path);
      if (!doc || doc.huge) throw new ApiError(404, 'not_found', `no such file: ${path}`);
      const a = Number(b.startLine);
      const z = Number(b.endLine);
      if (!(a >= 1) || z < a - 1 || z > doc.total) throw new ApiError(400, 'bad_request', 'startLine/endLine outside the file');
      const current = doc.lines.slice(a - 1, z).join('\n');
      if (current !== String(b.expected ?? '').replace(/\r\n/g, '\n')) {
        throw new ApiError(409, 'conflict', 'these lines changed on disk since you opened them', { current });
      }
      const next = b.text ? String(b.text).replace(/\r\n/g, '\n').split('\n') : [];
      doc.lines.splice(a - 1, z - a + 1, ...next);
      doc.total = doc.lines.length;
      doc.text = doc.total ? `${doc.lines.join('\n')}\n` : '';
      doc.html = null;
      setTimeout(() => emit('fs', { changes: [{ path, kind: 'modify' }] }), 20);
      return { path, lines: doc.total, startLine: a, endLine: a + next.length - 1, mtimeMs: Date.now() };
    },
    'GET file/markdown': async (q) => {
      const path = need(q, 'path');
      const text = await repo.text(path);
      if (text == null) throw new ApiError(404, 'not_found', `${path} not found`);
      const r = renderMarkdown(text, { path, rawUrl: (p) => new URL(`../${p}`, document.baseURI).toString() });
      return { path, ...r };
    },
    'GET settings/schema': () => ({ keys: MOCK_SCHEMA }),
    'GET file/outline': async (q) => {
      const path = need(q, 'path');
      const doc = await repo.doc(path);
      if (!doc || doc.huge) return { path, source: 'regex', symbols: [] };
      return { path, source: 'regex', symbols: outline(doc) };
    },
    // -------- Code navigation (F5 / B6), word-based over this repo's own web/src JS --------
    'GET symbols': async (q) => {
      const query = (q.q || '').toLowerCase();
      const limit = Math.min(100, Math.max(1, parseInt(q.limit || '50', 10)));
      const scored = [];
      for (const sym of await symbolIndex()) {
        const name = sym.name.toLowerCase();
        const at = query ? name.indexOf(query) : 0;
        if (at < 0) continue;
        scored.push({ ...sym, endLine: sym.line, score: (name === query ? 1000 : 0) + (at === 0 ? 100 : 0) - name.length });
      }
      scored.sort((a, b) => b.score - a.score || a.name.localeCompare(b.name));
      return { q: q.q || '', ms: 1, symbols: scored.slice(0, limit) };
    },
    'GET nav/definition': async (q) => {
      const { word } = await wordAt(q);
      const defs = (await symbolIndex()).filter((d) => d.name === word);
      if (!defs.length) throw new ApiError(404, 'not_found', 'no definition here');
      defs.sort((a, b) => (b.path === q.path) - (a.path === q.path));
      return { definitions: defs.slice(0, 10).map((d) => ({ path: d.path, line: d.line, endLine: d.line, kind: d.kind, name: d.name, source: 'mock' })), ms: 1 };
    },
    'GET nav/references': async (q) => {
      const { word } = await wordAt(q);
      const limit = Math.min(1000, Math.max(1, parseInt(q.limit || '200', 10)));
      const re = new RegExp(`(?<![\\w$])${word.replace(/\$/g, '\\$')}(?![\\w$])`, 'g');
      const references = [];
      for (const path of NAV_FILES) {
        const doc = await repo.doc(path);
        doc?.lines.forEach((line, i) => {
          for (const m of line.matchAll(re)) references.push({ path, line: i + 1, col: m.index + 1 });
        });
      }
      return { references: references.slice(0, limit), truncated: references.length > limit, ms: 2 };
    },
    'GET nav/hover': async (q) => {
      const { word } = await wordAt(q);
      const def = (await symbolIndex()).find((d) => d.name === word);
      if (!def) throw new ApiError(404, 'not_found', 'no definition here');
      const doc = await repo.doc(def.path);
      const comment = [];
      for (let i = def.line - 2; i >= 0 && /^\s*(\/\/|\*|\/\*\*)/.test(doc.lines[i]); i--) comment.unshift(doc.lines[i].replace(/^\s*(\/\*\*|\/\/|\*\/?)\s?/, ''));
      return { name: def.name, kind: def.kind, path: def.path, line: def.line, signature: doc.lines[def.line - 1].trim().replace(/\s*\{$/, ''), doc: comment.join('\n').trim() || null };
    },
    'POST highlight': (q, b) => {
      const lang = b.language || (b.path ? languageFor(b.path) : null);
      return { language: lang, lines: highlight(b.code || '', lang) };
    },
    'GET file/find': async (q) => {
      const path = need(q, 'path');
      const doc = await repo.doc(path);
      if (!doc || doc.huge) return { total: 0, truncated: false, matches: [] };
      const re = compile(q);
      const matches = [];
      doc.lines.forEach((line, i) => {
        const ranges = findRanges(line, re);
        if (ranges.length) matches.push({ line: i + 1, ranges });
      });
      return { total: matches.reduce((n, m) => n + m.ranges.length, 0), truncated: false, matches };
    },
    'GET fuzzy': (q) => {
      const t0 = performance.now();
      const r = fuzzyRank(q.q || '', repo.entries, Math.min(200, parseInt(q.limit || '50', 10)), (q.boost || '').split(',').filter(Boolean));
      return { q: q.q || '', total: r.total, ms: performance.now() - t0, generation: state.index.generation, results: q.q ? r.results : [] };
    },
    'GET search': async (q) => {
      const t0 = performance.now();
      const re = compile(q);
      const maxFiles = Math.min(LIMITS.maxSearchFiles, parseInt(q.maxFiles || '200', 10));
      const maxPerFile = parseInt(q.maxPerFile || '20', 10);
      const files = [];
      let scanned = 0;
      for (const e of repo.entries) {
        if (e.synthetic || e.path === MockRepo.HUGE_PATH || e.path === MockRepo.LARGE_PATH || repo.meta(e.path)?.kind !== 'text') continue;
        if (e.size > 2_000_000) continue;
        const doc = await repo.doc(e.path);
        scanned++;
        if (!doc) continue;
        const hits = [];
        let more = false;
        for (let i = 0; i < doc.lines.length; i++) {
          const ranges = findRanges(doc.lines[i], re);
          if (!ranges.length) continue;
          if (hits.length >= maxPerFile) { more = true; break; }
          hits.push(snippet(doc.lines[i], i + 1, ranges));
        }
        if (hits.length) files.push({ path: e.path, hits, more });
        if (files.length >= maxFiles) break;
      }
      return {
        q: q.q, engine: 'scan', ms: performance.now() - t0, filesScanned: scanned, filesMatched: files.length,
        truncated: files.length >= maxFiles, excluded: { globs: [], files: 0 }, files,
      };
    },
    'POST paths/resolve': (q, b) => {
      const resolved = {};
      for (const c of b.candidates || []) {
        const m = /^(.+?)(?::(\d+)(?::(\d+))?|#L(\d+)(?:-L(\d+))?)?$/.exec(c);
        const p = m?.[1] || c;
        let hit = repo.byPath.get(p);
        if (!hit) {
          const matches = repo.entries.filter((e) => e.path.endsWith(`/${p}`));
          hit = matches.length === 1 ? matches[0] : null;
        }
        resolved[c] = hit ? { path: hit.path, line: +(m?.[2] || m?.[4]) || undefined, endLine: +(m?.[5]) || undefined } : null;
      }
      return { resolved };
    },
    'GET git/status': () => repo.gitStatus(),
    'GET git/changes': (q) => (agentEdits.has(q.base) ? snapChanges(q.base) : repo.gitChanges(q.base, q.target)),
    'GET git/diff': (q) => {
      const e = agentEdits.get(q.base);
      if (!e) return repo.gitDiff(q);
      return { path: q.path, status: 'M', binary: false, tooLarge: false, language: e.lang, hunks: q.path === e.path ? liveHunks(e) : [] };
    },
    // -------- Checks (API.md § 16) --------
    'GET checks/breaking': (q) => (repo.isShowcaseCommit(q.target) ? {
      base: q.base, baseSha: q.base, target: q.target, targetSha: q.target, scanned: 3, indexed: true,
      changes: [{ name: 'score', qualified: 'score', kind: 'function', change: 'signature', public: true, oldLine: 39, oldSignature: 'pub fn score(query: &str, path: &str) -> Option<i64> {', newLine: 39, newSignature: 'pub fn score(query: &str, path: &str, opts: &ScoreOpts) -> Option<i64> {', path: 'crates/ferro-core/src/fuzzy.rs', severity: 'medium', stillDefined: false,
        refs: { count: 2, outsideFile: 2, common: false, inChangedFiles: 1, sample: [{ path: 'crates/ferro-core/src/search.rs', line: 88, col: 22 }, { path: 'crates/ferro-cli/src/main.rs', line: 64, col: 17 }] } }],
    } : {
      base: q.base || 'HEAD', baseSha: '9377e11a4c1f0f5d2d8b0a6f1e2d3c4b5a697886', target: q.target || 'worktree', targetSha: null, scanned: 12, indexed: true,
      changes: [
        { name: 'score_path', qualified: 'score_path', kind: 'function', change: 'removed', public: true, oldLine: 42, oldSignature: 'pub fn score_path(query: &str, path: &str) -> i64 {', path: 'crates/ferro-core/src/fuzzy.rs', severity: 'high', stillDefined: false,
          refs: { count: 3, outsideFile: 2, common: false, inChangedFiles: 1, sample: [{ path: 'crates/ferro-server/src/v1/files.rs', line: 118, col: 17 }, { path: 'crates/ferro-cli/src/main.rs', line: 64, col: 9 }, { path: 'crates/ferro-core/src/fuzzy.rs', line: 210, col: 13 }] } },
        { name: 'rank', qualified: 'Ranker::rank', kind: 'method', change: 'signature', public: true, oldLine: 88, oldSignature: 'pub fn rank(&self, q: &str) -> Vec<Hit> {', newLine: 90, newSignature: 'pub fn rank(&self, q: &str, limit: usize) -> Vec<Hit> {', path: 'crates/ferro-core/src/fuzzy.rs', severity: 'medium', stillDefined: false,
          refs: { count: 1, outsideFile: 1, common: false, inChangedFiles: 0, sample: [{ path: 'crates/ferro-server/src/v1/search.rs', line: 77, col: 21 }] } },
        { name: 'normalize', qualified: 'normalize', kind: 'function', change: 'renamed', newName: 'normalize_query', public: false, oldLine: 12, oldSignature: 'fn normalize(q: &str) -> String {', newLine: 12, newSignature: 'fn normalize_query(q: &str) -> String {', path: 'crates/ferro-core/src/fuzzy.rs', severity: 'info', stillDefined: false,
          refs: { count: 0, outsideFile: 0, common: false, inChangedFiles: 0, sample: [] } },
      ],
    }),
    'GET checks/tests/plan': () => ({
      steps: [{ runner: 'cargo', argv: ['cargo', 'test', '-p', 'ferro-core', '-p', 'ferro-server'], cwd: '', reason: 'the packages with changed files, and the packages that depend on them' }],
      allowed: true, refusal: null, againstWorktree: true,
    }),
    'POST checks/tests/run': () => {
      const id = `j_tests_${Date.now()}`;
      const job = { id, kind: 'checks.tests', state: 'running', startedAt: new Date().toISOString(), progress: { steps: ['cargo test -p ferro-core -p ferro-server'], step: 0, message: 'running cargo test -p ferro-core -p ferro-server' } };
      setTimeout(() => emit('job', { ...job }), 30);
      setTimeout(() => {
        Object.assign(job, { state: 'done', endedAt: new Date().toISOString(), result: { ok: false, passed: 211, failed: 1, skipped: 2, steps: [{
          runner: 'cargo', command: 'cargo test -p ferro-core -p ferro-server', cwd: '', exitCode: 101, ms: 8421, timedOut: false, cancelled: false, passed: 211, failed: 1, skipped: 2,
          failures: [{ name: 'fuzzy::tests::ranks_basename_first', path: 'crates/ferro-core/src/fuzzy.rs', line: 212, message: 'assertion `left == right` failed: left: "src/a.rs" right: "a.rs"' }],
          outputTail: 'test fuzzy::tests::ranks_basename_first ... FAILED\n\ntest result: FAILED. 211 passed; 1 failed; 2 ignored',
        }] } });
        emit('job', { ...job });
      }, 250);
      return { job: { id, kind: 'checks.tests' } };
    },
    'GET checks/coverage': (q) => {
      // Uncovered: the first two added lines of the first changed Rust file in the mock diff.
      const target = q.target || 'worktree';
      const first = repo.gitChanges(q.base || 'HEAD', target).files.find((f) => f.path.endsWith('.rs'));
      const d = first ? repo.gitDiff({ path: first.path, base: q.base || 'HEAD', target }) : { hunks: [] };
      const added = d.hunks.flatMap((hk) => hk.rows.filter((r) => r.t === 'add').map((r) => r.n));
      const miss = added.slice(0, 2);
      const hit = added.slice(2);
      return {
        report: { path: 'lcov.info', format: 'lcov', mtimeMs: Date.now() }, stale: false,
        files: first ? [{ path: first.path, inReport: true, added: added.length, covered: hit.length, uncovered: miss, coveredLines: hit }, { path: 'crates/ferro-core/src/new_mod.rs', inReport: false, added: 12, covered: 0, uncovered: [], coveredLines: [] }] : [],
        totals: { executable: added.length, covered: hit.length, uncovered: miss.length },
        percent: added.length ? Math.round((hit.length * 1000) / added.length) / 10 : null,
      };
    },
    'GET checks/security': (q) => (repo.isShowcaseCommit(q.target) ? { scanned: 3, findings: [], tools: [{ name: 'osv-scanner', installed: true }, { name: 'cargo-audit', installed: true }, { name: 'npm audit', installed: true }, { name: 'semgrep', installed: false }] } : {
      scanned: 12,
      findings: withMemory([
        { rule: 'secret.github-token', category: 'secret', severity: 'critical', title: 'GitHub token', detail: 'Revoke it at github.com/settings/tokens and read it from the environment.', path: 'crates/ferro-cli/src/main.rs', line: 31, excerpt: 'const TOKEN: &str = "ghp_…";', tool: 'ferro' },
        { rule: 'js.html-sink', category: 'code', severity: 'medium', title: 'HTML injection sink', detail: 'Setting HTML from strings is an XSS risk. Use textContent or build nodes; sanitize if HTML is required.', path: 'web/src/features/home.js', line: 88, excerpt: `card.inner${'HTML'} = item.title;`, tool: 'ferro' },
      ]),
      tools: [{ name: 'osv-scanner', installed: true }, { name: 'cargo-audit', installed: false }, { name: 'npm audit', installed: true }, { name: 'semgrep', installed: false }],
    }),
    'POST checks/security/deep': () => {
      const id = `j_sec_${Date.now()}`;
      const job = { id, kind: 'checks.security', state: 'running', startedAt: new Date().toISOString(), progress: { message: 'osv-scanner: checking dependencies' } };
      setTimeout(() => emit('job', { ...job }), 30);
      setTimeout(() => {
        Object.assign(job, { state: 'done', endedAt: new Date().toISOString(), result: {
          findings: [{ rule: 'RUSTSEC-2020-0071', category: 'dependency', severity: 'medium', tool: 'osv-scanner', title: 'time@0.1.43: Potential segfault in the time crate', detail: 'Known vulnerability RUSTSEC-2020-0071 (https://osv.dev/RUSTSEC-2020-0071). Upgrade time to a fixed version.', path: 'Cargo.lock', line: 0, excerpt: '' }],
          tools: [{ name: 'osv-scanner', status: 'ran', detail: '1 advisories' }, { name: 'semgrep', status: 'missing', detail: 'install semgrep for rule-based code scanning' }],
        } });
        emit('job', { ...job });
      }, 200);
      return { job: { id, kind: 'checks.security' } };
    },
    // -------- Team review memory (API.md § 17) --------
    'GET memory': () => ({
      rules: [...mockMemory.team.map((r) => ({ ...r, scope: 'team', hits: 0 })), ...mockMemory.personal.map((r) => ({ ...r, scope: 'personal', hits: 0 }))],
      suggestions: [...learnedSuggestions(), ...(mockMemory.dismissed.includes(MOCK_SUGGESTION.key) || [...mockMemory.team, ...mockMemory.personal].some((r) => r.title === 'Magic number') ? [] : [MOCK_SUGGESTION])],
      team: { path: '.ferro-rules.json', exists: mockMemory.team.length > 0, source: 'worktree', gitIgnored: false },
      signals: 2,
      forge: { provider: 'github', host: 'github.com', repo: 'aniketshukla1/ferro' },
      learned: { count: mockMemory.learnedAt ? MOCK_LEARNED.length : 0, at: mockMemory.learnedAt },
    }),
    'POST memory/learn': (q, b) => {
      const prs = Math.min(100, Math.max(5, Number(b?.prs) || 50));
      const id = `j_learn_${Date.now()}`;
      const job = { id, kind: 'memory.learn', state: 'running', startedAt: new Date().toISOString(), progress: { stage: 'fetch', repo: 'aniketshukla1/ferro', prs } };
      (state.jobs ||= {})[id] = job;
      setTimeout(() => emit('job', { ...job }), 30);
      setTimeout(() => { job.progress = { stage: 'ai', repo: 'aniketshukla1/ferro', prs, comments: 38 }; emit('job', { ...job }); }, 250);
      setTimeout(() => {
        mockMemory.learnedAt = new Date().toISOString();
        Object.assign(job, { state: 'done', endedAt: mockMemory.learnedAt, progress: undefined, result: { prs, comments: 38, conventions: MOCK_LEARNED.length, suggestions: learnedSuggestions().length, provider: 'anthropic', model: 'claude-sonnet-5' } });
        emit('job', { ...job });
      }, 600);
      return { job: { id, kind: 'memory.learn' } };
    },
    'POST memory/rules': (q, b) => {
      if (b.kind === 'ignore' && !b.rule && !b.category && !b.title) throw new ApiError(400, 'bad_request', 'an ignore rule needs a rule id, a category or a title');
      if (b.kind === 'convention' && !b.text?.trim()) throw new ApiError(400, 'bad_request', 'a convention needs its text');
      const rule = { id: `r_${Date.now().toString(16)}`, kind: b.kind, appliesTo: b.appliesTo, rule: b.rule, category: b.category, title: b.title, paths: b.paths || [], text: b.text, reason: b.reason || '', author: 'Aniket Shukla <aniket@example.com>', createdAt: new Date().toISOString() };
      (b.scope === 'team' ? mockMemory.team : mockMemory.personal).push(rule);
      return { ...rule, scope: b.scope === 'team' ? 'team' : 'personal' };
    },
    'PATCH memory/rules/{id}': (q, b) => {
      const from = mockMemory.team.find((r) => r.id === q._id) ? mockMemory.team : mockMemory.personal;
      const i = from.findIndex((r) => r.id === q._id);
      if (i < 0) throw new ApiError(404, 'not_found', 'no such rule');
      const [rule] = from.splice(i, 1);
      if (b.reason != null) rule.reason = b.reason;
      (b.scope === 'team' || (b.scope == null && from === mockMemory.team) ? mockMemory.team : mockMemory.personal).push(rule);
      return rule;
    },
    'DELETE memory/rules/{id}': (q) => {
      for (const list of [mockMemory.team, mockMemory.personal]) {
        const i = list.findIndex((r) => r.id === q._id);
        if (i >= 0) { list.splice(i, 1); return null; }
      }
      throw new ApiError(404, 'not_found', 'no such rule');
    },
    'POST memory/signals': () => null,
    'POST memory/suggestions/dismiss': (q, b) => { mockMemory.dismissed.push(b.key); return null; },
    // -------- AI change notes (API.md § 10.8) --------
    'POST ai/explain': async (q, b) => {
      await new Promise((r) => setTimeout(r, 60));
      if (repo.isShowcaseCommit(b.target)) {
        const notes = showcaseExplain(b.path);
        if (notes) return notes;
      }
      const d = repo.gitDiff({ path: b.path, base: b.base, target: b.target });
      const kindOf = (hk) => { const a = hk.rows.some((r) => r.t === 'add'); const del = hk.rows.some((r) => r.t === 'del'); return a && !del ? 'added' : del && !a ? 'removed' : 'changed'; };
      return {
        path: b.path,
        summary: `Tightens ${b.path.split('/').pop()}: clearer names and one edge case handled.`,
        hunks: (d.hunks || []).map((hk, i) => ({ id: hk.id, kind: kindOf(hk), note: i % 2 ? 'Renames the helper so its purpose is obvious at call sites.' : 'Handles the empty-input case before the loop, so it no longer panics.',
          ...(i % 3 === 1 ? { verdict: 'improve', why: 'Name the magic number so the next reader knows where it comes from.' } : { verdict: 'ok' }) })),
        cached: false,
      };
    },
    'POST git/hunk': (q, b) => {
      const e = agentEdits.get(b.base);
      if (!e || b.path !== e.path || !e.hunks.some((hk) => hk.id === b.hunkId)) throw new ApiError(404, 'not_found', 'unknown hunk');
      if (b.target !== 'worktree' || b.action !== 'revert') throw new ApiError(400, 'bad_request', "target must be 'worktree' and action 'revert'");
      e.reverted.add(b.hunkId);
      return repo.gitStatus();
    },
    'GET harness': () => harnessState(),
    'PUT harness': (q, b) => {
      if (b.id == null || b.id === '') { harness.selected = null; harness.model = ''; return harnessState(); }
      const x = harness.harnesses.find((hh) => hh.id === b.id);
      if (!x) throw new ApiError(400, 'bad_request', `unknown harness: ${b.id}`);
      if (!x.installed) throw new ApiError(422, 'unsupported', `${x.label} is not installed`);
      harness.selected = x.id;
      harness.model = b.model || '';
      return harnessState();
    },
    'POST harness/edit': async (q, b) => {
      if (!harness.selected) throw new ApiError(422, 'unsupported', 'no coding agent opted in');
      if (b.harness && b.harness !== harness.selected) throw new ApiError(422, 'unsupported', 'harness does not match the opted-in one');
      const range = { path: b.path, start: b.startLine, end: b.endLine };
      for (const [jobId, r] of agentRunning) {
        if (r.path === range.path && r.start <= range.end && range.start <= r.end) throw new ApiError(409, 'conflict', 'an edit is running on these lines', { jobId });
      }
      const doc = await repo.doc(b.path);
      if (!doc || doc.huge) throw new ApiError(404, 'not_found', `no such file: ${b.path}`);
      const id = `j_he_${Date.now().toString(36)}`;
      agentRunning.set(id, range);
      const job = { id, kind: 'harness.edit', state: 'running', startedAt: new Date().toISOString(), progress: { message: `running ${harness.selected}` } };
      state.jobs = state.jobs || {};
      state.jobs[id] = job;
      setTimeout(() => emit('job', { ...job }), 50);
      const instruction = b.instruction || '';
      const delay = /slow/.test(instruction) ? 4000 : 500;
      jobCancel.set(id, () => { agentRunning.delete(id); Object.assign(job, { state: 'cancelled', endedAt: new Date().toISOString(), result: { changed: [], base: 'snap_none', exitCode: 137, stdoutTail: '', stderrTail: '', stdoutTruncated: false, stderrTruncated: false, timedOut: false, ms: 10 } }); emit('job', { ...job }); });
      setTimeout(() => {
        if (job.state !== 'running') return;
        agentRunning.delete(id);
        const base = `snap_${id}`;
        const common = { stdoutTruncated: false, stderrTruncated: false, timedOut: false, ms: delay - 20 };
        if (/fail/.test(instruction)) {
          Object.assign(job, { state: 'failed', error: { code: 'internal', message: 'agent exited with code 1' }, result: { changed: [], base, exitCode: 1, stdoutTail: '', stderrTail: 'error: could not apply the edit', ...common } });
        } else if (/nothing/.test(instruction)) {
          Object.assign(job, { state: 'done', result: { changed: [], base, exitCode: 0, stdoutTail: 'No changes needed.', stderrTail: '', ...common } });
        } else {
          agentEdits.set(base, { jobId: id, path: b.path, lang: doc.lang, hunks: agentHunks(doc, range.start, range.end, instruction), reverted: new Set() });
          Object.assign(job, { state: 'done', result: { changed: [b.path], base, exitCode: 0, stdoutTail: `Edited ${b.path}`, stderrTail: '', ...common } });
        }
        job.endedAt = new Date().toISOString();
        emit('job', { ...job });
      }, delay);
      return { job: { id, kind: 'harness.edit' } };
    },
    // -------- Language servers (API.md § 4.9) --------
    'GET lsp/open': (q) => {
      const path = q.path || '';
      const lang = /\.rs$/.test(path) ? 'rust' : null;
      if (!lang || lspOpened.has(path)) return { opened: !!lang, enabled: true };
      lspOpened.add(path);
      // A language server answers a moment after the file opens.
      setTimeout(() => { if (MOCK_DIAGNOSTICS[path]) emit('diagnostics', { path, count: MOCK_DIAGNOSTICS[path].length }); }, 120);
      return { opened: true, enabled: true };
    },
    'GET diagnostics': () => {
      const files = [...lspOpened].filter((p) => MOCK_DIAGNOSTICS[p]).sort().map((path) => ({ path, diagnostics: MOCK_DIAGNOSTICS[path] }));
      const counts = { error: 0, warning: 0, info: 0, hint: 0 };
      for (const f of files) for (const d of f.diagnostics) counts[d.severity]++;
      return {
        enabled: true,
        servers: [
          { language: 'rust', command: 'rust-analyzer', state: lspOpened.size ? 'ready' : 'idle' },
          { language: 'go', command: 'gopls', state: 'missing' },
          { language: 'typescript', command: 'typescript-language-server', state: 'missing' },
          { language: 'python', command: 'pyright-langserver', state: 'missing' },
          { language: 'c', command: 'clangd', state: 'missing' },
        ],
        files,
        counts,
      };
    },
    // -------- Agent threads (API.md § 10.7) --------
    'GET harness/threads': () => ({
      threads: [...threads.values()].sort((a, b) => (a.updatedAt < b.updatedAt ? 1 : -1))
        .map((t) => ({ id: t.id, title: t.title, createdAt: t.createdAt, updatedAt: t.updatedAt, turns: t.turns.length, lastState: t.turns.at(-1)?.state ?? null })),
    }),
    'POST harness/threads': (q, b) => {
      const now = new Date().toISOString();
      const t = { id: `t_${Date.now().toString(36)}${Math.random().toString(36).slice(2, 8)}`, title: (b.title || '').trim() || 'New thread', createdAt: now, updatedAt: now, turns: [] };
      threads.set(t.id, t);
      return structuredClone(t);
    },
    'GET harness/threads/{id}': (q) => {
      const t = threads.get(q._id);
      if (!t) throw new ApiError(404, 'not_found', 'no such thread');
      return structuredClone(t);
    },
    'DELETE harness/threads/{id}': (q) => {
      if (!threads.delete(q._id)) throw new ApiError(404, 'not_found', 'no such thread');
      return null;
    },
    'POST harness/threads/{id}/turns': async (q, b) => {
      const t = threads.get(q._id);
      if (!t) throw new ApiError(404, 'not_found', 'no such thread');
      if (!harness.selected) throw new ApiError(422, 'unsupported', 'select a harness with PUT /api/v1/harness');
      const busy = t.turns.find((x) => x.state === 'running');
      if (busy) throw new ApiError(409, 'conflict', 'the previous turn is still running', { jobId: busy.jobId });
      const message = (b.message || '').trim();
      if (!message) throw new ApiError(400, 'bad_request', 'message is empty or too long');
      const target = b.context?.[0] || { path: 'web/src/core/bus.js', startLine: 6, endLine: 6 };
      const doc = await repo.doc(target.path);
      const id = `j_th_${Date.now().toString(36)}`;
      const turn = { id: `u_${Date.now().toString(36)}`, at: new Date().toISOString(), message, context: b.context || [], harness: harness.selected, model: harness.model, jobId: id, state: 'running' };
      if (!t.turns.length && t.title === 'New thread') t.title = message.split('\n')[0].slice(0, 80);
      t.turns.push(turn);
      t.updatedAt = turn.at;
      const job = { id, kind: 'harness.edit', state: 'running', startedAt: turn.at, progress: { threadId: t.id, turnId: turn.id } };
      state.jobs = state.jobs || {};
      state.jobs[id] = job;
      setTimeout(() => emit('job', { ...job }), 30);
      const delay = /slow/.test(message) ? 4000 : 400;
      const finish = (patch) => {
        Object.assign(job, patch, { endedAt: new Date().toISOString() });
        Object.assign(turn, { state: job.state, result: job.result, error: job.error });
        t.updatedAt = job.endedAt;
        emit('job', { ...job });
      };
      jobCancel.set(id, () => finish({ state: 'cancelled', result: { changed: [], base: 'snap_none', exitCode: 137, stdoutTail: '', stderrTail: '', ms: 5 } }));
      setTimeout(() => {
        if (job.state !== 'running') return;
        const base = `snap_${id}`;
        agentEdits.set(base, { jobId: id, path: target.path, lang: doc.lang, hunks: agentHunks(doc, target.startLine || 1, target.endLine || target.startLine || 1, message), reverted: new Set() });
        finish({ state: 'done', result: { changed: [target.path], base, exitCode: 0, stdoutTail: `Turn ${t.turns.length}: edited ${target.path}`, stderrTail: '', stdoutTruncated: false, stderrTruncated: false, timedOut: false, ms: delay - 10 } });
      }, delay);
      return { job: { id, kind: 'harness.edit' }, turn: structuredClone(turn) };
    },
    'POST harness/revert': (q, b) => {
      const e = [...agentEdits.values()].find((x) => x.jobId === b.jobId);
      if (!e) throw new ApiError(404, 'not_found', 'unknown job');
      for (const hk of e.hunks) e.reverted.add(hk.id);
      return { reverted: [e.path] };
    },
    'POST jobs/{id}/cancel': (q) => {
      const job = state.jobs?.[q._id];
      if (!job) throw new ApiError(404, 'not_found', 'unknown job');
      jobCancel.get(q._id)?.();
      return job;
    },
    'GET git/blob/lines': (q) => repo.gitBlobLines(q),
    'GET git/gutter': (q) => repo.gitGutter(q),
    'POST git/stage': (q, b) => {
      const s = repo.gitStage(b.paths || []);
      emit('git', s);
      return s;
    },
    'POST git/unstage': (q, b) => {
      const s = repo.gitUnstage(b.paths || []);
      emit('git', s);
      return s;
    },
    'POST git/discard': (q, b) => {
      const s = repo.gitDiscard(b.paths || [], b.confirm);
      emit('git', s);
      return s;
    },
    'POST git/commit': (q, b) => {
      const res = repo.gitCommit(b.message, b.amend);
      emit('git', res.status);
      return res;
    },
    'POST git/push': () => {
      const res = repo.gitPush();
      emit('git', res.status);
      return res;
    },
    'POST git/pull': () => {
      const res = repo.gitPull();
      emit('git', res.status);
      return res;
    },
    'GET git/log': (q) => repo.gitLogPage(q),
    // -------- History (API.md § 6.7–6.10) --------
    'GET git/refs': () => {
      const { main, topic } = repo.history();
      const cur = repo.branch || 'main';
      const b = (name, c, extra = {}) => ({ name, sha: c.sha, date: c.date, subject: c.subject, ahead: 0, behind: 0, current: false, ...extra });
      return {
        head: { branch: cur, sha: main[0].sha, detached: false },
        branches: [b(cur, cur === 'feature/search' ? topic[0] : main[0], { current: true, upstream: `origin/${cur}` }), ...(cur === 'feature/search' ? [b('main', main[0])] : [b('feature/search', topic[0], { upstream: 'origin/feature/search' })])],
        remotes: [b('origin/main', main[0]), b('origin/feature/search', topic[0])],
        tags: [b('v0.1.0', main[22])],
      };
    },
    'GET git/show': (q) => {
      const { main, topic } = repo.history();
      const c = [...main, ...topic].find((x) => x.sha.startsWith(q.rev) || x.refs.includes(q.rev));
      if (!c) throw new ApiError(400, 'bad_request', `bad revision: ${q.rev}`);
      return { ...c, committer: c.author, committerDate: c.date, body: c.subject.startsWith('feat') ? 'Why: faster path ranking for deep trees.\n\nMeasured on kubernetes: 1.8 ms.' : '', base: c.parents[0] || '4b825dc642cb6eb9a060e54bf8d69288fbee4904' };
    },
    'POST git/checkout': (q, b) => {
      const st = repo.gitStatus();
      if (st.counts.staged + st.counts.unstaged + st.counts.conflicted) throw new ApiError(409, 'conflict', `uncommitted changes in ${st.counts.staged + st.counts.unstaged} file(s): commit or stash them first`);
      repo.branch = b.ref.replace(/^origin\//, '');
      const status = repo.gitStatus();
      emit('git', status);
      return { output: `Switched to branch '${repo.branch}'`, status };
    },
    'POST git/fetch': () => ({ output: 'Fetching origin', status: repo.gitStatus() }),
    'GET metrics': () => metrics(bootedAt),
    // -------- Updates (API.md § 13): a release is found, installed, then "restarted" --------
    'GET update': () => ({ ...mockUpdate, auto: false }),
    'POST update/check': () => {
      Object.assign(mockUpdate, { state: 'available', latest: '0.3.0', checkedAt: Math.floor(Date.now() / 1000) });
      emit('update', { ...mockUpdate });
      return { ...mockUpdate, auto: false };
    },
    'POST update/install': () => {
      if (mockUpdate.state !== 'available') throw new ApiError(409, 'conflict', `nothing to install (update state: ${mockUpdate.state})`);
      Object.assign(mockUpdate, { state: 'downloading' });
      emit('update', { ...mockUpdate });
      setTimeout(() => { Object.assign(mockUpdate, { state: 'ready' }); emit('update', { ...mockUpdate }); }, 150);
      return { ...mockUpdate, auto: false };
    },
    'POST update/restart': () => {
      if (mockUpdate.state !== 'ready') throw new ApiError(409, 'conflict', 'no installed update is waiting for a restart');
      // The "new process": metrics report a fresh uptime, so the page reloads.
      setTimeout(() => { bootedAt = Date.now(); }, 300);
      return { restarting: true };
    },
    'GET jobs': () => ({ jobs: [] }),
    // Mock mode has no real session: sign-out just succeeds.
    'POST auth/logout': (q) => ({ ok: true, all: q.all === '1' || q.all === 'true' }),
    'POST index/rebuild': () => {
      const id = `j_${Date.now().toString(36)}`;
      emit('job', { id, kind: 'index.rebuild', state: 'running', startedAt: new Date().toISOString() });
      state.index = { ...state.index, state: 'indexing' };
      emit('index', state.index);
      setTimeout(() => {
        state.index = { ...state.index, state: 'ready', generation: state.index.generation + 1, ms: 16 + Math.round(Math.random() * 6) };
        emit('index', state.index);
        emit('job', { id, kind: 'index.rebuild', state: 'done', startedAt: new Date().toISOString(), endedAt: new Date().toISOString() });
      }, 300);
      return { job: { id, kind: 'index.rebuild' } };
    },

    // -------- PR / Review (F3) --------
    'GET pr': () => ({ pr: repo.pr }),
    'POST pr/open': (q, b) => {
      // Simulate a pr.open job: immediately done in mock
      repo.initPr();
      const id = `j_pro_${Date.now().toString(36)}`;
      const pr = repo.pr;
      const jobObj = { id, kind: 'pr.open', state: 'running', startedAt: new Date().toISOString(), progress: { message: 'metadata' }, result: null };
      state.jobs = state.jobs || {};
      state.jobs[id] = jobObj;
      setTimeout(() => {
        jobObj.progress = { message: 'fetch' };
        emit('job', { ...jobObj });
      }, 120);
      setTimeout(() => {
        jobObj.progress = { message: 'worktree' };
        emit('job', { ...jobObj });
      }, 240);
      setTimeout(() => {
        jobObj.state = 'done';
        jobObj.endedAt = new Date().toISOString();
        jobObj.result = { owner: pr.owner, repo: pr.repo, number: pr.number, title: pr.title, provider: pr.provider };
        state.jobs[id] = jobObj;
        emit('job', { ...jobObj });
        // emit workspace event so main.js can update pr store
        emit('pr', pr);
      }, 400);
      return { job: { id, kind: 'pr.open' } };
    },
    'GET jobs/{id}': (q) => {
      const id = q._id;
      return state.jobs?.[id] || { id, kind: 'unknown', state: 'done', startedAt: new Date().toISOString() };
    },
    'POST pr/refresh': () => {
      repo.pr = { ...repo.pr, headMoved: false };
      return repo.pr;
    },
    'GET pr/threads': () => ({ threads: repo.threads, conversation: repo.conversation }),
    'POST pr/threads/{id}/reply': (q, b) => {
      const thread = repo.threads.find((t) => t.id === q._id);
      if (!thread) throw new ApiError(404, 'not_found', 'thread not found');
      const c = { id: `c_${Date.now().toString(36)}`, author: { login: 'you' }, body: b.body || '', createdAt: new Date().toISOString() };
      (thread.comments = thread.comments || []).push(c);
      return c;
    },
    'POST pr/conversation': (q, b) => {
      const c = { id: `c_conv_${Date.now().toString(36)}`, author: { login: 'you' }, body: b.body || '', createdAt: new Date().toISOString() };
      repo.conversation.push(c);
      emit('threads', { conversation: repo.conversation });
      return c;
    },
    'GET review/drafts': () => ({ drafts: repo.drafts }),
    'POST review/drafts': (q, b) => {
      const now = new Date().toISOString();
      const d = { id: `d_${Date.now().toString(36)}`, path: b.path, line: b.line, startLine: b.startLine || b.line, side: b.side || 'RIGHT', body: b.body || '', threadId: b.threadId, source: b.source || 'human', stale: false, createdAt: now, updatedAt: now };
      repo.drafts.push(d);
      emit('drafts', { drafts: repo.drafts });
      return d;
    },
    'PATCH review/drafts/{id}': (q, b) => {
      const d = repo.drafts.find((x) => x.id === q._id);
      if (!d) throw new ApiError(404, 'not_found', 'draft not found');
      for (const k of ['body', 'line', 'startLine', 'side']) if (b[k] !== undefined) d[k] = b[k];
      d.updatedAt = new Date().toISOString();
      emit('drafts', { drafts: repo.drafts });
      return d;
    },
    'DELETE review/drafts/{id}': (q) => {
      repo.drafts = repo.drafts.filter((d) => d.id !== q._id);
      emit('drafts', { drafts: repo.drafts });
      return null;
    },
    'GET review/viewed': () => ({ headSha: repo.pr?.headSha || null, files: Object.fromEntries(repo.viewed) }),
    'PUT review/viewed': (q, b) => {
      const entry = repo.viewed.get(b.path) || { atSha: repo.pr?.headSha || '', changedSince: false };
      const updated = { ...entry, viewed: !!b.viewed };
      repo.viewed.set(b.path, updated);
      return updated;
    },
    'GET review/rounds': () => ({ rounds: repo.rounds }),
    'POST review/submit': (q, b) => {
      if (!repo.pr?.auth?.hasToken) throw new ApiError(401, 'unauthorized', 'no GitHub token', { hint: 'Run `gh auth login` or set GITHUB_TOKEN, then retry.' });
      const at = new Date().toISOString();
      repo.rounds.push({ headSha: repo.pr.headSha, at, kind: 'submitted' });
      // Stale drafts are skipped and stay (API.md § 9.2).
      const failed = repo.drafts.filter((d) => d.stale).map((d) => ({ draftId: d.id, error: 'line no longer exists at head' }));
      const posted = repo.drafts.length - failed.length;
      repo.drafts = repo.drafts.filter((d) => d.stale);
      emit('drafts', { drafts: repo.drafts });
      const url = `https://github.com/${repo.pr.owner}/${repo.pr.repo}/pull/${repo.pr.number}#pullrequestreview-${Date.now()}`;
      return { url, submittedAt: at, posted, failed };
    },
    'GET ai/status': () => ({
      configured: true,
      provider: 'anthropic',
      model: 'claude-sonnet-5',
      providers: [{ id: 'anthropic', label: 'Anthropic', configured: true, defaultModel: 'claude-sonnet-5' }],
    }),
    'POST markdown/render': (q, b) => renderMarkdown(b.text || '', { path: b.path || '', rawUrl: (p) => new URL(`../${p}`, document.baseURI).toString() }),
    'POST ai/review': (q, b) => {
      const id = `j_${Date.now().toString(36)}`;
      const findings = buildMockFindings(b);
      const job = { id, kind: 'ai.review', state: 'running', startedAt: new Date().toISOString() };
      emit('job', { ...job, progress: {} });
      let i = 0;
      const tick = () => {
        if (i < findings.length) {
          emit('job', { ...job, progress: { finding: findings[i] } });
          i++;
          setTimeout(tick, 140);
          return;
        }
        emit('job', {
          ...job,
          state: 'done',
          endedAt: new Date().toISOString(),
          result: {
            summary: findings.length ? `${plural(findings.length, 'finding')} across ${new Set(findings.map((f) => f.path)).size} file(s).` : 'No findings for the selected scope.',
            findings,
            usage: { inputTokens: 4180, outputTokens: 860, cacheReadTokens: 3120 },
          },
        });
      };
      setTimeout(tick, 140);
      return { job: { id, kind: 'ai.review' } };
    },
    'POST git/commit-message': () => {
      const staged = repo.gitStatus().files.filter((f) => f.index);
      if (!staged.length) throw new ApiError(409, 'conflict', 'Nothing staged. Stage changes before generating a message.');
      const scope = staged[0].path.split('/').pop().replace(/\.\w+$/, '');
      return { message: `feat(${scope}): ${plural(staged.length, 'staged file')} updated\n\nSummarized by the mock AI provider from the staged diff.` };
    },
  };


  // Compile route patterns once so dynamic segments like {id} work.
  const compiled = Object.keys(routes).map((key) => {
    const [m, ...rest] = key.split(' ');
    const tpl = rest.join(' ');
    const paramNames = [];
    const re = new RegExp('^' + tpl.replace(/\{([^}]+)\}/g, (_, n) => { paramNames.push(n); return '([^/]+)'; }) + '$');
    return { method: m, re, paramNames, handler: routes[key] };
  });

  async function request(path, { method = 'GET', query = {}, body, signal } = {}) {
    if (method === 'POST') {
      const fm = /^ai\/findings\/([^/]+)\/(accept|dismiss)$/.exec(path);
      if (fm) {
        await sleep(1.5 + Math.random() * 3);
        const found = MOCK_FINDING_SOURCE[Number(fm[1].replace('find_', ''))];
        if (!found) throw new ApiError(404, 'not_found', `no such finding: ${fm[1]}`);
        if (fm[2] === 'accept') return { data: { id: `draft_${fm[1]}`, path: found.path, line: found.line, side: found.side, body: (body || {}).body || found.suggestion || found.title, source: 'ai', createdAt: new Date().toISOString() }, clientMs: 0, serverMs: 0 };
        return { data: null, clientMs: 0, serverMs: 0 };
      }
    }
    const t0 = performance.now();
    if (opts.unauthorized) throw new ApiError(401, 'unauthorized', 'Open the link printed in your terminal');
    const q = {};
    for (const k in query) if (query[k] !== undefined && query[k] !== null) q[k] = String(query[k]);
    // Try exact match first, then parameterised patterns.
    let handler = routes[`${method} ${path}`];
    if (!handler) {
      for (const c of compiled) {
        if (c.method !== method) continue;
        const m = c.re.exec(path);
        if (m) {
          c.paramNames.forEach((n, i) => { q[`_${n}`] = decodeURIComponent(m[i + 1]); });
          handler = c.handler;
          break;
        }
      }
    }
    if (!handler) throw new ApiError(404, 'not_found', `mock: no route ${method} ${path}`);
    // realistic latency: localhost round trip + handler time
    await sleep(path === 'search' ? 18 + Math.random() * 20 : 1.5 + Math.random() * 3);
    if (signal?.aborted) throw new DOMException('aborted', 'AbortError');
    const s0 = performance.now();
    const data = await handler(q, body || {});
    const serverMs = performance.now() - s0;
    if (signal?.aborted) throw new DOMException('aborted', 'AbortError');
    return { data: JSON.parse(JSON.stringify(data)), clientMs: performance.now() - t0, serverMs };
  }


  function eventStream({ onEvent, onOpen, metrics: wantMetrics }) {
    const fn = (type, data) => onEvent(type, data);
    events.add(fn);
    const timers = [];
    timers.push(setTimeout(() => {
      onOpen();
      onEvent('hello', { api: 1, version: '0.2.0-dev', workspaceKey: 'a1b2c3d4e5f60718', generation: 1 });
      onEvent('index', state.index);
      onEvent('git', repo.gitStatus());
      onEvent('pr', repo.pr);
      if (wantMetrics) onEvent('metrics', metrics(started));

    }, 40));
    if (wantMetrics) timers.push(setInterval(() => onEvent('metrics', metrics(started)), 5000));
    return {
      close() {
        events.delete(fn);
        timers.forEach((t) => { clearTimeout(t); clearInterval(t); });
      },
    };
  }

  /** Mock of POST /ai/ask (streamed): times out fake `meta`/`tool_*`/`token`/`final`/`usage` events, abortable via `signal`. */
  function stream(path, body, { signal, onEvent }) {
    if (path === 'ai/edit') return streamEdit(body, { signal, onEvent });
    if (path !== 'ai/ask') return Promise.reject(new ApiError(404, 'not_found', `mock: no stream route ${path}`));
    return new Promise((resolve) => {
      const conversationId = body.conversationId || `c_${Date.now().toString(36)}`;
      const answerParts = buildMockAskAnswer(body.question, body.context?.path);
      const timers = [];
      let cancelled = false;
      const finish = () => { signal?.removeEventListener('abort', onAbort); resolve(); };
      const onAbort = () => { cancelled = true; timers.forEach((t) => clearTimeout(t)); finish(); };
      if (signal?.aborted) { onAbort(); return; }
      signal?.addEventListener('abort', onAbort, { once: true });

      const at = (ms, fn) => timers.push(setTimeout(() => { if (!cancelled) fn(); }, ms));
      let t = 30;
      at(t, () => onEvent('meta', { conversationId, provider: 'anthropic', model: 'claude-sonnet-5' }));
      if (body.context?.path) {
        t += 50;
        at(t, () => onEvent('tool_start', { id: 'tool_1', name: 'read_file', args: { path: body.context.path } }));
        t += 70;
        at(t, () => onEvent('tool_result', { id: 'tool_1', name: 'read_file', ok: true, output: `Read ${body.context.path}`, truncated: false, ms: 41 }));
      }
      for (const part of answerParts) {
        t += 35;
        at(t, () => onEvent('token', { text: part }));
      }
      t += 40;
      at(t, () => {
        onEvent('final', { text: answerParts.join(''), citations: body.context?.path ? [{ path: body.context.path, line: body.context.startLine || 1 }] : [] });
        onEvent('usage', { inputTokens: 1180, outputTokens: 240, cacheReadTokens: 860, cacheWriteTokens: 0 });
        finish();
      });
    });
  }

  /** Mock of POST /ai/edit (API.md § 10.9): a deterministic rewrite, streamed in small pieces. */
  function streamEdit(body, { signal, onEvent }) {
    if (!String(body.instruction || '').trim()) return Promise.reject(new ApiError(400, 'bad_request', 'instruction required'));
    if (/(^|\/)\.env/.test(body.path || '')) return Promise.reject(new ApiError(422, 'unsupported', 'this file matches ai.neverSend, so its contents are never sent to the AI provider'));
    const text = mockRewrite(body.text || '', body.instruction, body.path || '');
    const parts = text.match(/[\s\S]{1,14}/g) || [''];
    return new Promise((resolve) => {
      const timers = [];
      let cancelled = false;
      const finish = () => { signal?.removeEventListener('abort', onAbort); resolve(); };
      const onAbort = () => { cancelled = true; timers.forEach((t) => clearTimeout(t)); finish(); };
      if (signal?.aborted) { onAbort(); return; }
      signal?.addEventListener('abort', onAbort, { once: true });
      const at = (ms, fn) => timers.push(setTimeout(() => { if (!cancelled) fn(); }, ms));
      let t = 30;
      at(t, () => onEvent('meta', { provider: 'anthropic', model: 'claude-sonnet-5' }));
      for (const part of parts) { t += 18; at(t, () => onEvent('token', { text: part })); }
      at(t + 30, () => {
        onEvent('final', { text });
        onEvent('usage', { inputTokens: 1420, outputTokens: Math.ceil(text.length / 4), cacheReadTokens: 0, cacheWriteTokens: 0 });
        finish();
      });
    });
  }

  return {
    request,
    events: eventStream,
    stream,
    rawUrl: (path) => new URL(`../${path}`, document.baseURI).toString(),
    rawBlobUrl: (rev, path) => new URL(`../${path}`, document.baseURI).toString(),
    repo,
    emit,
  };
}

// ---------- helpers ----------

/** The mock AI's inline edit: "rename a to b", a doc comment, or a TODO note with the request. */
function mockRewrite(text, instruction, path) {
  const lines = text.split('\n');
  const indent = (lines.find((l) => l.trim()) || '').match(/^\s*/)[0];
  const note = /\.(py|sh|rb|ya?ml|toml)$/.test(path) ? '#' : /\.rs$/.test(path) ? '///' : '//';
  const rename = instruction.match(/rename\s+`?(\w+)`?\s+(?:to|as)\s+`?(\w+)`?/i);
  if (rename) return text.replace(new RegExp(`\\b${rename[1]}\\b`, 'g'), rename[2]);
  if (/\b(doc|comment|explain)/i.test(instruction)) {
    const first = (lines.find((l) => l.trim()) || '').trim();
    const name = first.match(/(?:fn|function|def|class|struct|const|let)\s+(\w+)/)?.[1];
    return `${indent}${note} ${name ? `\`${name}\`: ` : ''}${instruction.replace(/^(add|write)\s+(a\s+)?(doc\s+)?comment\s*/i, '').trim() || 'what this does and why'}.\n${text}`;
  }
  return `${indent}${note.replace('///', '//')} TODO: ${instruction.trim()}\n${text}`;
}

function metrics(started) {
  const t = (Date.now() - started) / 1000;
  return { rssBytes: Math.round((14.2 + Math.sin(t / 7) * 1.3 + Math.random() * 0.4) * 1048576), cpuPct: +(0.2 + Math.random() * 0.6).toFixed(1), threads: 9, uptimeMs: Date.now() - started };
}

function compile(q) {
  const text = q.q || '';
  const regex = q.mode === 'regex';
  let flags = 'g';
  const smart = (q.case || 'smart') === 'smart';
  if (q.case === 'insensitive' || (smart && text === text.toLowerCase())) flags += 'i';
  let src = regex ? text : text.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
  if (q.word === '1') src = `\\b(?:${src})\\b`;
  try {
    return new RegExp(src, flags);
  } catch (e) {
    throw new ApiError(400, 'bad_request', `invalid regex: ${e.message}`, { position: 0 });
  }
}

function findRanges(line, re) {
  re.lastIndex = 0;
  const out = [];
  let m;
  while ((m = re.exec(line))) {
    if (m[0].length === 0) { re.lastIndex++; continue; }
    out.push([m.index, m.index + m[0].length]);
    if (out.length > 50) break;
  }
  return out;
}

function snippet(line, n, ranges) {
  const MAX = 400;
  let start = 0;
  let cutStart = false;
  if (line.length > MAX && ranges[0][0] > 60) {
    start = ranges[0][0] - 40;
    cutStart = true;
  }
  const text = line.slice(start, start + MAX);
  const cutEnd = start + MAX < line.length;
  return {
    line: n,
    text,
    ranges: ranges.map(([a, b]) => [a - start, b - start]).filter(([a, b]) => a >= 0 && b <= text.length),
    cutStart,
    cutEnd,
  };
}

const OUTLINE = {
  rust: [[/^(\s*)(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?(?:unsafe\s+)?(?:const\s+)?fn\s+([A-Za-z_]\w*)/, 'function'], [/^(\s*)(?:pub(?:\([^)]*\))?\s+)?struct\s+([A-Za-z_]\w*)/, 'struct'], [/^(\s*)(?:pub(?:\([^)]*\))?\s+)?enum\s+([A-Za-z_]\w*)/, 'enum'], [/^(\s*)(?:pub(?:\([^)]*\))?\s+)?trait\s+([A-Za-z_]\w*)/, 'trait'], [/^(\s*)impl(?:<[^>]*>)?\s+(?:[\w:<>, ]+\s+for\s+)?([A-Za-z_][\w:]*)/, 'impl'], [/^(\s*)(?:pub(?:\([^)]*\))?\s+)?mod\s+([A-Za-z_]\w*)/, 'module'], [/^(\s*)(?:pub(?:\([^)]*\))?\s+)?(?:const|static)\s+([A-Z_][A-Z0-9_]*)/, 'const']],
  js: [[/^(\s*)(?:export\s+)?(?:default\s+)?(?:async\s+)?function\s*\*?\s*([A-Za-z_$][\w$]*)/, 'function'], [/^(\s*)(?:export\s+)?(?:default\s+)?class\s+([A-Za-z_$][\w$]*)/, 'class'], [/^(\s*)(?:export\s+)?(?:const|let)\s+([A-Za-z_$][\w$]*)\s*=\s*(?:async\s*)?(?:\([^)]*\)|[A-Za-z_$][\w$]*)\s*=>/, 'function'], [/^(\s+)(?:static\s+|async\s+|get\s+|set\s+)*([A-Za-z_$][\w$]*)\s*\([^)]*\)\s*\{\s*$/, 'method']],
  python: [[/^(\s*)(?:async\s+)?def\s+([A-Za-z_]\w*)/, 'function'], [/^(\s*)class\s+([A-Za-z_]\w*)/, 'class']],
  go: [[/^()func\s+(?:\([^)]*\)\s*)?([A-Za-z_]\w*)/, 'function'], [/^()type\s+([A-Za-z_]\w*)/, 'type']],
};
OUTLINE.ts = OUTLINE.js;

function outline(doc) {
  const out = [];
  if (doc.lang === 'markdown') {
    let fence = false;
    doc.lines.forEach((line, i) => {
      if (/^\s*```/.test(line)) fence = !fence;
      const m = !fence && /^(#{1,6})\s+(.*)$/.exec(line);
      if (m) out.push({ name: m[2].replace(/[`*_]/g, ''), kind: 'heading', line: i + 1, depth: m[1].length - 1 });
    });
    return out;
  }
  const pats = OUTLINE[doc.lang];
  if (!pats) return out;
  // Indent unit = smallest non-zero leading indent in the file (2-space JS, 4-space Rust/Python).
  let unit = 8;
  for (const line of doc.lines) {
    const w = /^( *)\S/.exec(line.replace(/\t/g, '    '))?.[1].length;
    if (w && w < unit) unit = w;
  }
  doc.lines.forEach((line, i) => {
    for (const [re, kind] of pats) {
      const m = re.exec(line);
      if (m && !NOT_SYMBOL.has(m[2])) {
        const indent = (m[1] || '').replace(/\t/g, '    ').length;
        out.push({ name: m[2], kind, line: i + 1, depth: Math.min(4, Math.floor(indent / unit)) });
        break;
      }
    }
  });
  return out;
}

// Control-flow keywords that look like `name(args) {` to the method regex.
const NOT_SYMBOL = new Set(['if', 'for', 'while', 'switch', 'catch', 'with', 'return', 'function', 'else', 'do', 'try', 'typeof', 'new', 'await', 'yield', 'super']);

export { hugeLine };
