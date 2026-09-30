// Problems (API.md § 4.9): diagnostics from language servers. Opened files go to their server;
// the Problems tab lists every file's diagnostics, the viewer marks the lines, the status bar
// shows the counts. Loads after boot when the server reports `lsp.diagnostics`.
import { h, mount } from '../core/dom.js';
import { request } from '../core/api.js';
import { bus } from '../core/bus.js';
import { store } from '../core/store.js';
import { basename, debounce, dirname, plural } from '../core/util.js';
import { icon, fileIcon } from '../ui/icons.js';

const SEV = { error: 0, warning: 1, info: 2, hint: 3 };
const LETTER = { error: 'E', warning: 'W', info: 'I', hint: 'H' };

let editorRef = null;

/** Keep diagnostics in sync: open files with their server, refetch on events, mark lines. */
export function installProblems({ editor }) {
  editorRef = editor;
  const opened = new Set();
  const openActive = (path) => {
    if (!path || opened.has(path)) return;
    opened.add(path);
    request('lsp/open', { query: { path } }).catch(() => opened.delete(path));
  };
  const refresh = debounce(async () => {
    try {
      const data = await request('diagnostics');
      store.set('diagnostics', data);
      store.set('diagCounts', data.counts);
      mark();
    } catch { /* offline: keep the last set */ }
  }, 250);
  store.subscribe('active', (p) => { openActive(p); mark(); }, { now: true });
  bus.on('ev:diagnostics', refresh);
  bus.on('ev:workspace', () => { opened.clear(); refresh(); });
  refresh();
}

/** Push the active file's diagnostics into its code view as line markers. */
function mark() {
  const view = editorRef?.activeView();
  const code = view?.sourceView ? view.sourceView() : view;
  if (!code?.setDiagnostics) return;
  const file = store.get('diagnostics')?.files?.find((f) => f.path === code.path);
  const map = new Map();
  for (const d of file?.diagnostics || []) {
    const cur = map.get(d.line);
    if (!cur || SEV[d.severity] < SEV[cur.severity]) map.set(d.line, { severity: d.severity, message: d.message });
  }
  code.setDiagnostics(map.size ? map : null);
}

export function renderProblemsTab(el, { onOpen }) {
  let filter = 'all';
  const seg = h('div', { class: 'seg', role: 'group', 'aria-label': 'Severity' },
    ['all', 'error', 'warning'].map((f) => h('button', { 'aria-pressed': String(f === filter), 'data-f': f, on: { click: () => { filter = f; render(); } } }, f === 'all' ? 'All' : f === 'error' ? 'Errors' : 'Warnings')));
  const summary = h('div', { class: 'pb-summary faint small', role: 'status' });
  const list = h('div', { class: 'pb-list' });
  const servers = h('div', { class: 'pb-servers faint small' });
  mount(el, h('div', { class: 'pb' }, h('div', { class: 'pb-head' }, h('span', { class: 'pb-title' }, 'Problems'), seg), summary, list, servers));

  function render() {
    for (const b of seg.children) b.setAttribute('aria-pressed', String(b.dataset.f === filter));
    const data = store.get('diagnostics');
    if (!data) { mount(list, h('p', { class: 'faint small' }, 'Loading…')); return; }
    if (!data.enabled) {
      mount(list, h('div', { class: 'pb-empty' }, h('p', null, 'Language servers are off for this workspace.'),
        h('p', { class: 'faint small' }, 'They run for local folders but not pull-request checkouts, whose code is untrusted. Settings → Editor → Language servers turns them on.')));
      summary.textContent = '';
      mount(servers);
      return;
    }
    const c = data.counts;
    summary.textContent = `${plural(c.error, 'error')} · ${plural(c.warning, 'warning')}${c.info + c.hint ? ` · ${c.info + c.hint} other` : ''}`;
    const keep = (d) => filter === 'all' || d.severity === filter;
    const files = data.files.map((f) => ({ ...f, diagnostics: f.diagnostics.filter(keep).sort((a, b) => SEV[a.severity] - SEV[b.severity] || a.line - b.line) })).filter((f) => f.diagnostics.length);
    mount(list, files.length ? files.map((f) => h('section', { class: 'pb-file' },
      h('button', { class: 'pb-file-head', title: f.path, on: { click: () => onOpen(f.path, { focus: true }) } }, fileIcon(f.path), h('span', { class: 'lr-name' }, basename(f.path)), h('span', { class: 'lr-dir' }, dirname(f.path)), h('span', { class: 'count' }, String(f.diagnostics.length))),
      f.diagnostics.map((d) => h('button', {
        class: `pb-row sev-${d.severity}`,
        on: { click: () => onOpen(f.path, { focus: true, line: d.line }) },
      }, h('span', { class: `pb-sev ${d.severity}`, 'aria-label': d.severity }, LETTER[d.severity]),
      h('span', { class: 'pb-msg' }, d.message),
      h('span', { class: 'pb-loc num faint' }, `${d.line}:${d.col}${d.source ? ` · ${d.source}` : ''}`)))))
      : h('div', { class: 'pb-empty' }, icon('check-circle', 'lg'), h('p', null, 'No problems in the files you opened.')));
    mount(servers, h('span', null, 'Servers: '), data.servers.filter((s) => s.state !== 'missing').map((s, i) => h('span', null, i ? ', ' : '', `${s.language} (${s.state})`)),
      data.servers.every((s) => s.state === 'missing') ? h('span', null, 'none found on PATH (rust-analyzer, gopls, typescript-language-server, pyright, pylsp, clangd)') : null);
  }
  store.subscribe('diagnostics', render);
  render();
}
