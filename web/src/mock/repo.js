// Mock repository: real file list snapshot plus synthetic content and PR review state.
// Follows API.md § 6, § 8, § 9.
import { FILES } from './files.js';
import { highlight, languageFor, LANGUAGE_NAMES } from './hl.js';
import { showcaseChanges, showcaseDiff } from './showcase.js';
import { fileKind } from '../ui/icons.js';
import { ApiError } from '../core/api.js';

const HUGE_PATH = 'generated/huge.log';
const HUGE_LINES = 400_000;
const LARGE_PATH = 'generated/large.log';
const LARGE_LINES = 12_000;
const IMAGE_EXT = /\.(png|jpe?g|gif|webp|svg|ico|icns|bmp)$/i;
const BINARY_EXT = /\.(png|jpe?g|gif|webp|ico|icns|bmp|woff2?|ttf|pdf|zip|gz)$/i;

export class MockRepo {
  constructor({ big = false } = {}) {
    this.entries = FILES.map(([path, size]) => ({ path, size, lower: path.toLowerCase() }));
    this.entries.push({ path: HUGE_PATH, size: 31_000_000, lower: HUGE_PATH });
    this.entries.push({ path: LARGE_PATH, size: 900_000, lower: LARGE_PATH });
    if (big) this.addSynthetic(60_000);
    this.byPath = new Map(this.entries.map((e) => [e.path, e]));
    this.buildDirs();
    this.git = this.makeGit();
    this.commits = [
      { sha: '9377e11a4c1f0f5d2d8b0a6f1e2d3c4b5a697886', short: '9377e11', subject: 'feat(core): initial tree', author: 'Aniket Shukla', date: '2026-09-25T12:00:00Z' },
      { sha: '8266d00b3c2e1f4d1c7a9b5e0d1c2b3a4f586775', short: '8266d00', subject: 'chore: setup pipeline', author: 'Aniket Shukla', date: '2026-09-24T10:00:00Z' },
    ];
    this.branch = 'main';
    this.ahead = 0;
    this.behind = 0;
    this.docs = new Map();
    // No PR is active until /pr/open runs (mock/server.js) — matches the real B4 backend,
    // where GET /api/v1/pr returns { pr: null } until a PR/MR has actually been opened.
    this.pr = null;
    this.threads = [];
    this.conversation = [];
    this.drafts = [];
    this.viewed = new Map();
    this.rounds = [];
  }

  /** Populate this repo's PR review state — called by mock/server.js's POST /pr/open. */
  initPr() {
    this.pr = {
      provider: 'github', host: 'github.com', owner: 'aniketshukla1', repo: 'ferro', number: 42,
      url: 'https://github.com/aniketshukla1/ferro/pull/42',
      title: 'feat(search): optimize trigram index and cache',
      bodyHtml: '<p>This pull request optimizes the search index with <code>trigram</code> hashing and LRU caching.</p>',
      author: { login: 'octocat' },
      state: 'open', draft: false,
      baseRef: 'main', headRef: 'feat/search-opt',
      baseSha: '9377e11a4c1f0f5d2d8b0a6f1e2d3c4b5a697886',
      headSha: 'a1b2c3d4e5f60718293a4b5c6d7e8f9012345678',
      mergeBaseSha: '9377e11a4c1f0f5d2d8b0a6f1e2d3c4b5a697886',
      isFork: false, createdAt: '2026-09-26T12:00:00Z', updatedAt: '2026-09-27T08:00:00Z',
      stats: { files: 3, additions: 45, deletions: 12, commits: 3 },
      checks: { state: 'success', url: 'https://github.com/aniketshukla1/ferro/actions' },
      auth: { hasToken: true, source: 'env', canReview: true, canPushHead: true },
      lastReviewedSha: null, headMoved: false,
    };
    this.threads = [
      {
        id: 'th_1', path: 'crates/ferro-core/src/search.rs', line: 12, side: 'RIGHT', outdated: false, resolved: false,
        comments: [{ id: 'c_1', author: { login: 'reviewer' }, body: 'Consider bumping timeout to 5000 ms.', createdAt: '2026-09-26T14:00:00Z' }],
      },
      {
        id: 'th_2', path: 'crates/ferro-core/src/search.rs', line: null, originalLine: 10, side: 'LEFT', outdated: true, resolved: true,
        comments: [{ id: 'c_2', author: { login: 'reviewer' }, body: 'Resolved comment on line 10.', createdAt: '2026-09-25T11:00:00Z' }],
      },
    ];
    this.conversation = [
      { id: 'c_conv1', author: { login: 'octocat' }, body: 'Ready for review! Verified against baseline.', createdAt: '2026-09-26T12:05:00Z' },
    ];
    this.drafts = [];
    this.viewed = new Map([
      ['crates/ferro-core/src/search.rs', { viewed: false, atSha: 'a1b2c3d', changedSince: false }],
      ['web/app.js', { viewed: true, atSha: 'a1b2c3d', changedSince: false }],
      ['README.md', { viewed: false, atSha: 'a1b2c3d', changedSince: false }],
    ]);
    this.rounds = [
      { headSha: '9377e11a4c1f0f5d2d8b0a6f1e2d3c4b5a697886', at: '2026-09-26T15:00:00Z', kind: 'submitted' },
    ];
  }

  addSynthetic(n) {
    const t = ['services', 'packages', 'libs', 'apps', 'tools'], m = ['auth', 'billing', 'search', 'render', 'storage'];
    const l = ['handler', 'client', 'server', 'model', 'util'], e = ['rs', 'ts', 'go', 'py', 'md', 'json'];
    for (let i = 0; i < n; i++) {
      const p = `synthetic/${t[i % 5]}/${m[(i >> 3) % 5]}/m${(i >> 7) % 97}/${l[i % 5]}_${i}.${e[i % 6]}`;
      this.entries.push({ path: p, size: 1200 + (i % 5000), lower: p.toLowerCase(), synthetic: true });
    }
  }

  buildDirs() {
    this.dirs = new Map([['', { dirs: new Set(), files: [] }]]);
    for (const e of this.entries) {
      const parts = e.path.split('/');
      let cur = '';
      for (let i = 0; i < parts.length - 1; i++) {
        const next = cur ? `${cur}/${parts[i]}` : parts[i];
        if (!this.dirs.has(next)) this.dirs.set(next, { dirs: new Set(), files: [] });
        this.dirs.get(cur).dirs.add(parts[i]);
        cur = next;
      }
      this.dirs.get(cur).files.push(e);
    }
  }

  makeGit() {
    const status = new Map();
    for (const e of this.entries) {
      if (/^(docs\/spec\/|bench\/|web\/src\/|web\/styles\/|web\/tests\/|web\/next\.html|web\/assets\/|\.claude\/)/.test(e.path)) {
        status.set(e.path, { path: e.path, index: null, worktree: null, untracked: true, conflicted: false });
      }
    }
    const tracked = ['crates/ferro-core/src/search.rs', 'crates/ferro-cli/src/server.rs', 'web/app.js', 'README.md'].filter((p) => this.byPath.has(p));
    // Diff fixtures (image, binary, too-large) are listed whether or not the tree has them.
    for (const p of [...tracked, 'web/assets/favicon.svg', 'assets/test.bin', 'generated/huge.diff']) {
      status.set(p, { path: p, index: null, worktree: 'M', untracked: false, conflicted: false });
    }
    // Deleted fixture: gone from disk, still in git (the tree shows it struck through).
    status.set('crates/ferro-core/src/retired.rs', { path: 'crates/ferro-core/src/retired.rs', index: null, worktree: 'D', untracked: false, conflicted: false });
    // Rename fixture: query.rs → engine.rs (covers §3.4 rename display requirement)
    status.set('crates/ferro-core/src/engine.rs', {
      path: 'crates/ferro-core/src/engine.rs',
      origPath: 'crates/ferro-core/src/query.rs',
      index: 'R',
      worktree: null,
      untracked: false,
      conflicted: false,
    });
    return status;
  }

  gitStatus() {
    const files = [...this.git.values()].map((f) => ({
      path: f.path, origPath: f.origPath, index: f.index, worktree: f.worktree, untracked: !!f.untracked, conflicted: !!f.conflicted,
    }));
    return {
      branch: this.branch || 'main', detached: false, headSha: '9377e11a4c1f0f5d2d8b0a6f1e2d3c4b5a697886',
      upstream: 'origin/main', ahead: this.ahead || 0, behind: this.behind || 0, files,
      counts: {
        staged: files.filter((f) => f.index).length,
        unstaged: files.filter((f) => f.worktree && !f.untracked).length,
        untracked: files.filter((f) => f.untracked).length,
        conflicted: files.filter((f) => f.conflicted).length,
      },
    };
  }

  gitStage(paths = []) {
    for (const p of paths) {
      let f = this.git.get(p);
      if (!f) this.git.set(p, { path: p, index: 'A', worktree: null, untracked: false, conflicted: false });
      else if (f.untracked) { f.index = 'A'; f.untracked = false; }
      else if (f.worktree) { f.index = f.worktree; f.worktree = null; }
    }
    return this.gitStatus();
  }

  gitUnstage(paths = []) {
    for (const p of paths) {
      const f = this.git.get(p);
      if (f?.index) {
        if (f.index === 'A' && !f.worktree) { f.untracked = true; f.index = null; }
        else { f.worktree = f.index; f.index = null; }
      }
    }
    return this.gitStatus();
  }

  gitDiscard(paths = [], confirm = false) {
    if (!confirm) throw new ApiError(400, 'bad_request', 'Missing confirmation');
    for (const p of paths) {
      const f = this.git.get(p);
      if (f) {
        if (f.untracked) this.git.delete(p);
        else if (f.worktree) { f.worktree = null; if (!f.index) this.git.delete(p); }
      }
    }
    return this.gitStatus();
  }

  gitCommit(message = '', amend = false) {
    const staged = [...this.git.values()].filter((f) => f.index);
    if (!staged.length && !amend) throw new ApiError(409, 'conflict', 'nothing to commit');
    const sha = Math.random().toString(16).slice(2, 10) + Math.random().toString(16).slice(2, 10);
    const summary = message.trim().split('\n')[0] || 'commit';
    this.commits.unshift({ sha, short: sha.slice(0, 7), subject: summary, author: 'Aniket Shukla', date: new Date().toISOString() });
    staged.forEach((f) => { f.index = null; if (!f.worktree) this.git.delete(f.path); });
    return { sha, summary, status: this.gitStatus() };
  }

  gitPush() { this.ahead = 0; return { output: 'Everything up-to-date\n', status: this.gitStatus() }; }
  gitPull() { this.behind = 0; return { output: 'Already up to date.\n', updated: false, status: this.gitStatus() }; }
  gitLog(limit = 50) { return { commits: this.commits.slice(0, +limit || 50) }; }

  /** History fixture (API.md § 6.6): main's commits plus 150 older ones, a topic branch and tags. */
  history() {
    const hex = (n) => (n * 2654435761 >>> 0).toString(16).padStart(8, '0').repeat(5);
    const subjects = ['fix(search): trim trailing whitespace', 'feat(tree): keyboard navigation', 'perf(index): reuse trigram buffers', 'docs: update README', 'refactor(git): split status parsing', 'test: cover rename detection'];
    const authors = [['Aniket Shukla', 'aniket@example.com'], ['Riya Rao', 'riya@example.com'], ['Sam Lee', 'sam@example.com']];
    const main = this.commits.map((c, i) => ({ ...c, email: 'aniket@example.com', parents: [], refs: i === 0 ? ['HEAD', this.branch || 'main', 'origin/main'] : [] }));
    for (let i = 0; i < 150; i++) {
      const [author, email] = authors[i % 3];
      main.push({ sha: hex(i + 7), short: hex(i + 7).slice(0, 7), parents: [], author, email, date: new Date(Date.parse('2026-09-23T10:00:00Z') - i * 3600e3 * 7).toISOString(), subject: `${subjects[i % subjects.length]} (#${400 - i})`, refs: i === 20 ? ['v0.1.0'] : [] });
    }
    for (let i = 0; i < main.length - 1; i++) main[i].parents = [main[i + 1].sha];
    const topic = [0, 1, 2].map((i) => ({ sha: hex(900 + i), short: hex(900 + i).slice(0, 7), parents: [], author: 'Riya Rao', email: 'riya@example.com', date: new Date(Date.parse('2026-09-26T09:00:00Z') - i * 3600e3).toISOString(), subject: ['feat(search): fuzzy scoring by path depth', 'feat(search): add scorer tests', 'chore(search): scaffold scorer'][i], refs: i === 0 ? ['feature/search', 'origin/feature/search'] : [] }));
    for (let i = 0; i < 2; i++) topic[i].parents = [topic[i + 1].sha];
    topic[2].parents = [main[1].sha];
    return { main, topic };
  }

  gitLogPage(q = {}) {
    const { main, topic } = this.history();
    let list = q.all === '1' || q.all === 'true' ? [...topic, ...main].sort((a, b) => (a.date < b.date ? 1 : -1)) : q.rev === 'feature/search' || q.rev === 'origin/feature/search' ? [...topic, ...main.slice(1)] : main;
    const s = (q.q || '').toLowerCase();
    const a = (q.author || '').toLowerCase();
    if (s) list = list.filter((c) => c.subject.toLowerCase().includes(s));
    if (a) list = list.filter((c) => c.author.toLowerCase().includes(a) || c.email.includes(a));
    const skip = +q.skip || 0;
    const limit = Math.min(+q.limit || 50, 500);
    return { commits: list.slice(skip, skip + limit), hasMore: list.length > skip + limit };
  }

  /** The feature/search tip ("fuzzy scoring by path depth", or a prefix of its sha): its views
   *  show the showcase change (showcase.js). Other commits keep the generic mock diff. */
  isShowcaseCommit(rev) {
    if (!rev || !/^[0-9a-f]{7,40}$/i.test(rev)) return false;
    return this.history().topic[0].sha.startsWith(rev);
  }

  gitChanges(base = 'HEAD', target = 'worktree') {
    if (this.isShowcaseCommit(target)) {
      const files = showcaseChanges();
      return { base, baseSha: base, target, targetSha: target, stats: { files: files.length, additions: files.reduce((s, f) => s + f.additions, 0), deletions: files.reduce((s, f) => s + f.deletions, 0) }, files };
    }
    if (base === 'test-50-files') {
      const files = Array.from({ length: 50 }, (_, i) => ({
        path: `src/m_${String(i).padStart(2, '0')}.rs`, status: i % 4 === 0 ? 'A' : (i % 4 === 1 ? 'D' : 'M'),
        additions: 10 + (i * 3) % 20, deletions: 3 + (i * 2) % 10, binary: false,
      }));
      return { base: 'HEAD', baseSha: '9377e11a4c1f0f5d2d8b0a6f1e2d3c4b5a697886', target: 'worktree', targetSha: null,
        stats: { files: 50, additions: files.reduce((s, f) => s + f.additions, 0), deletions: files.reduce((s, f) => s + f.deletions, 0) }, files };
    }
    const status = this.gitStatus();
    const files = status.files.map((f) => ({
      path: f.path, oldPath: f.origPath, status: f.untracked ? '?' : (f.index || f.worktree || 'M'),
      additions: f.untracked ? 25 : (f.path.endsWith('.rs') ? 14 : 5),
      deletions: f.untracked ? 0 : 3,
      binary: f.path.endsWith('.bin'),
    }));
    return {
      base: base || 'HEAD', baseSha: '9377e11a4c1f0f5d2d8b0a6f1e2d3c4b5a697886', target: target || 'worktree', targetSha: null,
      stats: { files: files.length, additions: files.reduce((s, f) => s + f.additions, 0), deletions: files.reduce((s, f) => s + f.deletions, 0) }, files,
    };
  }

  gitDiff(q = {}) {
    const { path, force } = q;
    if (this.isShowcaseCommit(q.target)) {
      const d = showcaseDiff(path);
      if (d) return d;
    }
    const isForce = force === 1 || force === '1' || force === true;
    if ((path?.includes('tooLarge') || path === 'generated/huge.diff') && !isForce) {
      return { path, status: 'M', binary: false, tooLarge: true, language: 'rust', hunks: [] };
    }
    if (path?.endsWith('.bin')) return { path, status: 'M', binary: true, tooLarge: false, language: null, hunks: [] };
    // Like git (and the real backend): raster images are binary; SVG is text.
    if (/\.(png|jpg|jpeg|gif|webp|ico)$/i.test(path || '')) {
      return { path, status: 'M', binary: true, tooLarge: false, language: null, oldBlob: 'sha_old_img', newBlob: 'sha_new_img', hunks: [] };
    }
    if (/\.svg$/i.test(path || '')) {
      return { path, status: 'M', binary: false, tooLarge: false, language: null, oldBlob: 'sha_old_img', newBlob: 'sha_new_img', hunks: [] };
    }
    if (path === 'generated/benchmark-20k.diff' || path?.includes('20k')) {
      const hunks = Array.from({ length: 100 }, (_, h) => {
        const s = h * 200 + 1;
        return {
          id: `hunk_${h}`, header: `@@ -${s},200 +${s},200 @@ fn s_${h}`, section: `fn s_${h}`, oldStart: s, oldLines: 200, newStart: s, newLines: 200,
          rows: Array.from({ length: 200 }, (_, r) => {
            const m = r % 5;
            if (!m) return { t: 'ctx', o: s + r, n: s + r, html: `<span class="t-cm">// ctx ${r}</span>`, text: `// ctx ${r}` };
            if (m < 3) return { t: 'del', o: s + r, n: null, html: `<span class="t-kw">let</span> v_${r}=<span class="t-num">${r}</span>;`, text: `let v_${r}=${r};`, ch: [[18, 20]] };
            return { t: 'add', o: null, n: s + r, html: `<span class="t-kw">let</span> v_${r}=<span class="t-num">${r + 100}</span>;`, text: `let v_${r}=${r + 100};`, ch: [[18, 20]] };
          }),
        };
      });
      return { path, status: 'M', binary: false, tooLarge: false, language: 'rust', hunks };
    }

    const st = this.git.get(path);
    if (st?.worktree === 'D') {
      const gone = ['pub fn retired(q: &str) -> Vec<String> {', '    Vec::new()', '}'];
      return {
        path, status: 'D', binary: false, tooLarge: false, language: 'rust',
        hunks: [{ id: 'hunk_1', header: '@@ -1,3 +0,0 @@', section: '', oldStart: 1, oldLines: 3, newStart: 0, newLines: 0,
          rows: gone.map((text, i) => ({ t: 'del', o: i + 1, n: null, text, html: text.replace(/&/g, '&amp;').replace(/</g, '&lt;') })) }],
      };
    }
    return {
      path: path || 'crates/ferro-core/src/search.rs',
      ...(st?.origPath ? { oldPath: st.origPath } : {}),
      status: st?.index || st?.worktree || 'M',
      binary: false,
      tooLarge: false,
      language: 'rust',
      hunks: [
        {
          id: 'hunk_1',
          header: '@@ -10,12 +10,14 @@ fn search_engine',
          section: 'fn search_engine',
          oldStart: 10,
          oldLines: 12,
          newStart: 10,
          newLines: 14,
          rows: [
            { t: 'ctx', o: 10, n: 10, html: '<span class="t-cm">// Initialize search context</span>', text: '// Initialize search context' },
            { t: 'del', o: 11, n: null, html: '<span class="t-kw">let</span> <span class="t-id">max_results</span>: <span class="t-id">usize</span> = <span class="t-num">50</span>;', text: 'let max_results: usize = 50;', ch: [[29, 31]] },
            { t: 'add', o: null, n: 11, html: '<span class="t-kw">let</span> <span class="t-id">max_results</span>: <span class="t-id">usize</span> = <span class="t-num">100</span>;', text: 'let max_results: usize = 100;', ch: [[29, 32]] },
            { t: 'del', o: 12, n: null, html: '<span class="t-kw">let</span> <span class="t-id">timeout</span> = <span class="t-num">2000</span>;', text: 'let timeout = 2000;', ch: [[18, 22]] },
            { t: 'add', o: null, n: 12, html: '<span class="t-kw">let</span> <span class="t-id">timeout</span> = <span class="t-num">5000</span>;', text: 'let timeout = 5000;', ch: [[18, 22]] },
            { t: 'add', o: null, n: 13, html: '<span class="t-kw">let</span> <span class="t-id">fuzzy</span> = <span class="t-kw">true</span>;', text: 'let fuzzy = true;' },
            { t: 'ctx', o: 13, n: 14, html: '<span class="t-kw">let</span> <span class="t-id">engine</span> = <span class="t-id">Engine</span>::<span class="t-fn">new</span>();', text: 'let engine = Engine::new();' },
            { t: 'ctx', o: 14, n: 15, html: '<span class="t-id">engine</span>.<span class="t-fn">start</span>();', text: 'engine.start();' },
          ],
        },
        {
          id: 'hunk_2',
          header: '@@ -30,3 +31,4 @@ fn search_engine',
          section: 'fn search_engine',
          oldStart: 30,
          oldLines: 3,
          newStart: 31,
          newLines: 4,
          rows: [
            { t: 'ctx', o: 30, n: 31, html: '<span class="t-cm">// Result processing</span>', text: '// Result processing' },
            // Emoji line fixture (§3.4): ensures the renderer handles multi-byte Unicode characters
            { t: 'add', o: null, n: 32, html: '<span class="t-cm">// \uD83D\uDE80 launch search with emoji label</span>', text: '// \uD83D\uDE80 launch search with emoji label' },
            // CRLF line (§3.4): like the backend, text keeps the trailing \r and html never includes the terminator
            { t: 'ctx', o: 31, n: 33, html: '<span class="t-id">results</span>.<span class="t-fn">push</span>(<span class="t-id">hit</span>);', text: 'results.push(hit);\r' },
            { t: 'ctx', o: 32, n: 34, html: '<span class="t-cm">// done</span>', text: '// done' },
          ],
        },
      ],
    };
  }

  gitBlobLines(q = {}) {
    const f = +q.from || 1, c = +q.count || 20;
    const lines = Array.from({ length: c }, (_, i) => ({ n: f + i, text: `// context line ${f + i}`, html: `<span class="t-cm">// context line ${f + i}</span>` }));
    return { rev: q.rev || 'HEAD', from: f, count: c, total: 1000, lines };
  }

  gitGutter(q = {}) {
    const isChanged = this.git.has(q.path) || q.path === 'crates/ferro-core/src/search.rs' || q.path?.includes('diff');
    return isChanged ? { path: q.path || '', base: q.base || 'HEAD', added: [11, 13], modified: [12], deleted: [10] }
      : { path: q.path || '', base: q.base || 'HEAD', added: [], modified: [], deleted: [] };
  }

  isDirty(dir) {
    const pre = `${dir}/`;
    for (const [p, g] of this.git.entries()) {
      if (p.startsWith(pre) && (g.worktree || g.index || g.untracked)) return true;
    }
    return false;
  }

  children(dir) {
    const d = this.dirs.get(dir);
    if (!d) return null;
    const dirs = [...d.dirs].sort((a, b) => a.localeCompare(b, undefined, { sensitivity: 'base' })).map((name) => ({
      name, path: dir ? `${dir}/${name}` : name, dir: true, dirty: this.isDirty(dir ? `${dir}/${name}` : name) || undefined,
    }));
    const files = [...d.files].sort((a, b) => a.path.localeCompare(b.path, undefined, { sensitivity: 'base' })).map((e) => {
      const g = this.git.get(e.path);
      return {
        name: e.path.slice(dir ? dir.length + 1 : 0), path: e.path, dir: false, size: e.size,
        git: g ? (g.untracked ? '?' : (g.worktree || g.index || null)) : undefined,
      };
    });
    return [...dirs, ...files];
  }

  async text(path) {
    const e = this.byPath.get(path);
    if (!e || path === HUGE_PATH) return null;
    if (!e.synthetic) {
      try {
        const res = await fetch(new URL(`../${path}`, document.baseURI));
        const ct = res.headers.get('content-type') || '';
        if (res.ok && !ct.includes('text/html')) return await res.text();
      } catch { /* fallback */ }
    }
    return generated(path);
  }

  async doc(path) {
    if (this.docs.has(path)) return this.docs.get(path);
    if (path === HUGE_PATH) {
      const d = { path, huge: true, total: HUGE_LINES, lang: 'log' };
      this.docs.set(path, d);
      return d;
    }
    if (path === LARGE_PATH) {
      const lines = Array.from({ length: LARGE_LINES }, (_, i) => {
        const idx = i + 1;
        return (idx === 10 || idx === 11000) ? `Line ${idx}: FERRO_CANARY_512KB needle match marker` : `Line ${idx}: telemetry record ${idx}`;
      });
      const norm = lines.join('\n');
      const d = { path, text: norm, lines, total: LARGE_LINES, lang: 'log', html: null, eol: 'lf' };
      this.docs.set(path, d);
      return d;
    }
    const t = await this.text(path);
    if (t == null) return null;
    const norm = t.replace(/\r\n?/g, '\n');
    const lines = norm.endsWith('\n') ? norm.slice(0, -1).split('\n') : norm.split('\n');
    const d = { path, text: norm, lines, total: norm.length ? lines.length : 0, lang: languageFor(path), html: null, eol: t.includes('\r\n') ? 'crlf' : 'lf' };
    this.docs.set(path, d);
    return d;
  }

  highlighted(doc) {
    if (!doc.html) doc.html = highlight(doc.text, doc.lang);
    return doc.html;
  }

  meta(path) {
    const e = this.byPath.get(path);
    if (!e) return null;
    return { e, kind: IMAGE_EXT.test(path) ? 'image' : BINARY_EXT.test(path) ? 'binary' : 'text' };
  }

  languageName(path) {
    const l = languageFor(path);
    return (l && LANGUAGE_NAMES[l]) || fileKind(path).language || null;
  }

  static get HUGE_PATH() { return HUGE_PATH; }
  static get LARGE_PATH() { return LARGE_PATH; }
}

const LEVELS = ['INFO', 'DEBUG', 'WARN', 'ERROR'];
export function hugeLine(i) {
  return `${new Date(Date.UTC(2026, 8, 24) + i * 137).toISOString()} ${LEVELS[i % 4]} ferro::server request id=${i.toString(16)}`;
}

export function hugeLineHtml(i) {
  const l = hugeLine(i);
  return `<span class="t-n">${l.slice(0, 24)}</span> <span class="t-k">${LEVELS[i % 4]}</span> ${l.slice(30)}`;
}

function generated(path) {
  const name = path.split('/').pop(), lang = languageFor(path);
  if (lang === 'markdown') return `# ${name}\n\nGenerated mock content.\n`;
  if (lang === 'json') return '{\n  "name": "synthetic"\n}\n';
  return `// ${name}\nexport function f(id) { return { id }; }\n`;
}
