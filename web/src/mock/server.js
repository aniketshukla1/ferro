// Mock implementation of the API v1 surface the frontend uses today.
// Shapes follow docs/spec/API.md exactly so switching to the real backend is a no-op.
import { ApiError } from '../core/api.js';
import { storage, sleep } from '../core/util.js';
import { MockRepo, hugeLine, hugeLineHtml } from './repo.js';
import { fuzzyRank } from './fuzzy.js';
import { renderMarkdown } from './markdown.js';

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
  'jobs', 'workspace.open', 'metrics', 'fuzzy.v2', 'search.v2', 'search.regex', 'file.find', 'paths.resolve', 'git.status.v2', 'auth.logout',
];
const LIMITS = { maxWindowLines: 1000, maxCols: 4000, maxRawBytes: 33554432, maxMarkdownBytes: 4194304, maxSearchFiles: 1000, maxDiffRows: 20000 };

export function createMockServer(opts) {
  const repo = new MockRepo({ big: opts.big });
  const started = Date.now();
  const state = {
    index: { state: 'indexing', files: 0, ms: 0, generation: 1, searchIndex: 'off' },
    settings: storage.get('ferro.mock.settings', {}),
    session: storage.get('ferro.mock.session', null),
  };
  const events = new Set();
  const emit = (type, data) => events.forEach((fn) => fn(type, data));

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
    pr: null,
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
        const html = repo.highlighted(doc);
        for (let n = from; n <= end; n++) {
          const raw = doc.lines[n - 1] ?? '';
          const line = { n, html: html[n - 1] ?? '' };
          if (raw.length > LIMITS.maxCols) line.cut = raw.length;
          lines.push(line);
        }
      }
      return { path, from, total: doc.total, mtimeMs: started, exact: true, language: repo.languageName(path), lines };
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
        if (e.synthetic || e.path === MockRepo.HUGE_PATH || repo.meta(e.path)?.kind !== 'text') continue;
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
    'GET metrics': () => metrics(started),
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
  };

  async function request(path, { method = 'GET', query = {}, body, signal } = {}) {
    const t0 = performance.now();
    if (opts.unauthorized) throw new ApiError(401, 'unauthorized', 'Open the link printed in your terminal');
    const q = {};
    for (const k in query) if (query[k] !== undefined && query[k] !== null) q[k] = String(query[k]);
    const route = routes[`${method} ${path}`];
    if (!route) throw new ApiError(404, 'not_found', `mock: no route ${method} ${path}`);
    // realistic latency: localhost round trip + handler time
    await sleep(path === 'search' ? 18 + Math.random() * 20 : 1.5 + Math.random() * 3);
    if (signal?.aborted) throw new DOMException('aborted', 'AbortError');
    const s0 = performance.now();
    const data = await route(q, body || {});
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

  return {
    request,
    events: eventStream,
    rawUrl: (path) => new URL(`../${path}`, document.baseURI).toString(),
    repo,
    emit,
  };
}

// ---------- helpers ----------
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
