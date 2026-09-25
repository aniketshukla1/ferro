// Mock repository: this repo's real file list (files.js snapshot) plus synthetic content.
// When served from the repo root (http://127.0.0.1:4173/web/next.html?mock=1) file
// contents are fetched from disk; otherwise plausible content is generated.
import { FILES } from './files.js';
import { highlight, languageFor, LANGUAGE_NAMES } from './hl.js';
import { fileKind } from '../ui/icons.js';

const HUGE_PATH = 'generated/huge.log';
const HUGE_LINES = 400_000;
const IMAGE_EXT = /\.(png|jpe?g|gif|webp|svg|ico|icns|bmp)$/i;
const BINARY_EXT = /\.(png|jpe?g|gif|webp|ico|icns|bmp|woff2?|ttf|pdf|zip|gz)$/i;

export class MockRepo {
  constructor({ big = false } = {}) {
    this.entries = FILES.map(([path, size]) => ({ path, size, lower: path.toLowerCase() }));
    this.entries.push({ path: HUGE_PATH, size: 31_000_000, lower: HUGE_PATH });
    if (big) this.addSynthetic(60_000);
    this.byPath = new Map(this.entries.map((e) => [e.path, e]));
    this.buildDirs();
    this.git = this.makeGit();
    this.docs = new Map();
  }

  addSynthetic(n) {
    const tops = ['services', 'packages', 'libs', 'apps', 'tools'];
    const mids = ['auth', 'billing', 'search', 'render', 'storage', 'gateway', 'scheduler', 'metrics', 'ui', 'core'];
    const leaves = ['handler', 'client', 'server', 'model', 'util', 'config', 'index', 'types', 'store', 'queue', 'worker', 'router'];
    const exts = ['rs', 'ts', 'go', 'py', 'md', 'json', 'tsx'];
    for (let i = 0; i < n; i++) {
      const t = tops[i % 5];
      const m = mids[(i >> 3) % 10];
      const sub = `m${(i >> 7) % 97}`;
      const leaf = leaves[i % 12];
      const path = `synthetic/${t}/${m}/${sub}/${leaf}_${i}.${exts[i % 7]}`;
      this.entries.push({ path, size: 1200 + (i % 5000), lower: path.toLowerCase(), synthetic: true });
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
      if (/^(docs\/spec\/|bench\/|web\/src\/|web\/styles\/|web\/tests\/|web\/next\.html|web\/assets\/|\.claude\/)/.test(e.path)) status.set(e.path, '?');
    }
    for (const p of ['crates/ferro-core/src/search.rs', 'crates/ferro-cli/src/server.rs', 'web/app.js', 'README.md']) {
      if (this.byPath.has(p)) status.set(p, 'M');
    }
    return status;
  }

  gitStatus() {
    const files = [...this.git].map(([path, code]) => ({
      path,
      index: null,
      worktree: code === '?' ? null : code,
      untracked: code === '?',
      conflicted: false,
    }));
    return {
      branch: 'main', detached: false, headSha: '9377e11a4c1f0f5d2d8b0a6f1e2d3c4b5a697886',
      upstream: 'origin/main', ahead: 0, behind: 0,
      files,
      counts: {
        staged: 0,
        unstaged: files.filter((f) => f.worktree).length,
        untracked: files.filter((f) => f.untracked).length,
        conflicted: 0,
      },
    };
  }

  isDirty(dir) {
    const pre = `${dir}/`;
    for (const p of this.git.keys()) if (p.startsWith(pre)) return true;
    return false;
  }

  children(dir) {
    const d = this.dirs.get(dir);
    if (!d) return null;
    const dirs = [...d.dirs].sort((a, b) => a.localeCompare(b, undefined, { sensitivity: 'base' })).map((name) => {
      const path = dir ? `${dir}/${name}` : name;
      return { name, path, dir: true, dirty: this.isDirty(path) || undefined };
    });
    const files = [...d.files].sort((a, b) => a.path.localeCompare(b.path, undefined, { sensitivity: 'base' })).map((e) => ({
      name: e.path.slice(dir ? dir.length + 1 : 0),
      path: e.path,
      dir: false,
      size: e.size,
      git: this.git.get(e.path),
    }));
    return [...dirs, ...files];
  }

  async text(path) {
    const e = this.byPath.get(path);
    if (!e) return null;
    if (path === HUGE_PATH) return null;
    if (!e.synthetic) {
      try {
        const res = await fetch(new URL(`../${path}`, document.baseURI));
        const ct = res.headers.get('content-type') || '';
        if (res.ok && !ct.includes('text/html')) return await res.text();
      } catch { /* fall through to generated content */ }
    }
    return generated(path);
  }

  /** Document model for a text file (cached). */
  async doc(path) {
    if (this.docs.has(path)) return this.docs.get(path);
    if (path === HUGE_PATH) {
      const doc = { path, huge: true, total: HUGE_LINES, lang: 'log' };
      this.docs.set(path, doc);
      return doc;
    }
    const text = await this.text(path);
    if (text == null) return null;
    const norm = text.replace(/\r\n?/g, '\n');
    const lines = norm.endsWith('\n') ? norm.slice(0, -1).split('\n') : norm.split('\n');
    const lang = languageFor(path);
    const doc = { path, text: norm, lines, total: norm.length ? lines.length : 0, lang, html: null, eol: text.includes('\r\n') ? 'crlf' : 'lf' };
    this.docs.set(path, doc);
    return doc;
  }

  highlighted(doc) {
    if (!doc.html) doc.html = highlight(doc.text, doc.lang);
    return doc.html;
  }

  meta(path) {
    const e = this.byPath.get(path);
    if (!e) return null;
    const kind = IMAGE_EXT.test(path) ? 'image' : BINARY_EXT.test(path) ? 'binary' : 'text';
    return { e, kind };
  }

  languageName(path) {
    const lang = languageFor(path);
    return (lang && LANGUAGE_NAMES[lang]) || fileKind(path).language || null;
  }

  static get HUGE_PATH() { return HUGE_PATH; }
}

// ---------- synthetic content ----------
const LEVELS = ['INFO', 'INFO', 'INFO', 'DEBUG', 'WARN', 'INFO', 'ERROR', 'INFO'];
const MSGS = [
  'request completed', 'cache hit for key', 'index generation bumped', 'watcher event coalesced',
  'search finished', 'fuzzy ranked candidates', 'highlight window served', 'git status refreshed',
];

export function hugeLine(i) {
  const t = new Date(Date.UTC(2026, 8, 24, 9, 0, 0) + i * 137).toISOString();
  const lvl = LEVELS[i % LEVELS.length];
  return `${t} ${lvl.padEnd(5)} ferro::${['server', 'index', 'search', 'git'][i % 4]} ${MSGS[(i * 7) % MSGS.length]} id=${(i * 2654435761 >>> 0).toString(16)} ms=${((i * 13) % 97) / 10}`;
}

export function hugeLineHtml(i) {
  const line = hugeLine(i);
  const m = line.match(/^(\S+) (\S+)(\s+)(\S+)(.*)$/);
  if (!m) return line;
  const lvlCls = m[2] === 'ERROR' ? 't-del' : m[2] === 'WARN' ? 't-at' : m[2] === 'DEBUG' ? 't-c' : 't-k';
  const rest = m[5].replace(/(\w+)=([\w.]+)/g, '<span class="t-pr">$1</span><span class="t-o">=</span><span class="t-n">$2</span>');
  return `<span class="t-n">${m[1]}</span> <span class="${lvlCls}">${m[2]}</span>${m[3]}<span class="t-ns">${m[4]}</span>${rest}`;
}

function generated(path) {
  const name = path.split('/').pop();
  const lang = languageFor(path);
  if (lang === 'markdown') return `# ${name}\n\nGenerated content for mock mode.\n`;
  if (lang === 'json') return '{\n  "name": "synthetic",\n  "version": "0.1.0"\n}\n';
  const base = name.replace(/\.\w+$/, '');
  if (lang === 'rust') return `//! ${base}: synthetic module\n\npub struct ${cap(base)} {\n    id: u64,\n}\n\nimpl ${cap(base)} {\n    pub fn new(id: u64) -> Self {\n        Self { id }\n    }\n}\n`;
  if (lang === 'go') return `package ${base.split('_')[0]}\n\n// ${cap(base)} is synthetic.\ntype ${cap(base)} struct {\n\tID int\n}\n\nfunc New${cap(base)}(id int) *${cap(base)} {\n\treturn &${cap(base)}{ID: id}\n}\n`;
  if (lang === 'python') return `"""${base}: synthetic module."""\n\n\nclass ${cap(base)}:\n    def __init__(self, id: int) -> None:\n        self.id = id\n`;
  return `// ${name}\nexport function ${base.replace(/\W/g, '_')}(id) {\n  return { id };\n}\n`;
}

const cap = (s) => s.replace(/(^|_)(\w)/g, (_, __, c) => c.toUpperCase());
