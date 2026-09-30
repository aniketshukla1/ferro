// Sidebar panels beyond the file tree: Changes, Search, Outline.
import { h, mount, markRanges } from '../core/dom.js';
import { api, has, isAbort } from '../core/api.js';
import { store } from '../core/store.js';
import { bus } from '../core/bus.js';
import { debounce, basename, dirname, formatMs, plural } from '../core/util.js';
import { icon, fileIcon, folderIcon } from '../ui/icons.js';
import { focusHit } from '../core/text.js';
import { revealDir } from './tree.js';
// ---------- Changes ----------
export async function renderChangesPanel(section, opts) {
  const { renderChangesPanel: render } = await import('./git.js');
  return render(section, opts);
}

// ---------- Search ----------
export function renderSearchPanel(section, { onOpen }) {
  const input = h('input', { class: 'input', type: 'search', placeholder: 'Search in files', 'aria-label': 'Search in files', spellcheck: 'false' });
  const caseBtn = h('button', { class: 'icon-btn sm opt', 'aria-pressed': 'false', 'aria-label': 'Match case', 'data-tip': 'Match case' }, h('span', { class: 'opt-t' }, 'Aa'));
  const wordBtn = h('button', { class: 'icon-btn sm opt', 'aria-pressed': 'false', 'aria-label': 'Whole word', 'data-tip': 'Whole word' }, h('span', { class: 'opt-t' }, 'ab'));
  const reBtn = h('button', { class: 'icon-btn sm opt', 'aria-pressed': 'false', 'aria-label': 'Regular expression', 'data-tip': 'Regular expression' }, h('span', { class: 'opt-t' }, '.*'));
  for (const b of [caseBtn, wordBtn, reBtn]) b.addEventListener('click', () => { b.setAttribute('aria-pressed', String(b.getAttribute('aria-pressed') !== 'true')); run(); });
  const summary = h('div', { class: 'search-summary' });
  const results = h('div', { class: 'panel-body search-results' });
  mount(section,
    h('div', { class: 'search-box first' }, h('div', { class: 'search-input' }, input, h('div', { class: 'search-opts' }, caseBtn, wordBtn, reBtn)), summary),
    results);

  let ctrl = null;
  let seq = 0;
  async function run() {
    const q = input.value;
    ctrl?.abort();
    const my = ++seq;
    if (q.length < 2) { mount(results); summary.textContent = ''; return; }
    ctrl = new AbortController();
    summary.replaceChildren(h('span', { class: 'spinner' }), ' Searching…');
    const params = {
      q,
      mode: reBtn.getAttribute('aria-pressed') === 'true' ? 'regex' : 'literal',
      case: caseBtn.getAttribute('aria-pressed') === 'true' ? 'sensitive' : 'smart',
      word: wordBtn.getAttribute('aria-pressed') === 'true' ? 1 : undefined,
      maxFiles: 200,
      maxPerFile: 50,
    };
    try {
      const res = await api.search(params, { signal: ctrl.signal });
      if (my !== seq) return;
      const hitCount = res.files.reduce((n, f) => n + f.hits.length, 0);
      summary.textContent = res.files.length ? `${plural(hitCount, 'result')} in ${plural(res.files.length, 'file')} · ${formatMs(res.ms)}${res.truncated ? ' · truncated' : ''}` : `No results · ${formatMs(res.ms)}`;
      mount(results, res.files.map((f) => {
        const hits = h('div', { class: 'sr-hits' }, f.hits.map((hit) => h('button', {
          class: 'sr-hit',
          on: { click: () => onOpen(f.path, { preview: true, focus: false, line: hit.line }) },
        }, h('span', { class: 'sr-ln num' }, String(hit.line)), (() => { const f = focusHit(hit.text, hit.ranges); return h('span', { class: 'sr-text' }, markRanges(f.text, f.ranges)); })())));
        const head = h('button', { class: 'sr-file', title: f.path, on: { click: () => { hits.hidden = !hits.hidden; head.classList.toggle('collapsed', hits.hidden); } } },
          icon('chevron-down', 'xs'), fileIcon(f.path), h('span', { class: 'lr-name' }, basename(f.path)), h('span', { class: 'lr-dir' }, dirname(f.path)), h('span', { class: 'count' }, String(f.hits.length) + (f.more ? '+' : '')));
        return h('div', { class: 'sr-group' }, head, hits);
      }));
    } catch (e) {
      if (!isAbort(e) && my === seq) summary.textContent = e.message;
    }
  }
  input.addEventListener('input', debounce(run, 120));
  input.addEventListener('keydown', (e) => { if (e.key === 'Enter') run(); });
  return {
    /** Focus the box; `q` replaces the query, `word` sets the whole-word toggle (Alt+U usages). */
    focus(q, { word } = {}) {
      if (typeof q === 'string' && q) input.value = q;
      if (word != null) wordBtn.setAttribute('aria-pressed', String(!!word));
      input.focus();
      input.select();
      if (input.value) run();
    },
  };
}

// ---------- Outline ----------
const KIND_ICON = { function: 'zap', method: 'zap', class: 'braces', struct: 'braces', enum: 'list-tree', interface: 'braces', trait: 'braces', type: 'braces', module: 'layers', const: 'hash', var: 'hash', field: 'hash', impl: 'layers', macro: 'sparkles', heading: 'hash' };

export function renderOutlinePanel(section, { editor }) {
  const filter = h('input', { class: 'input', type: 'search', placeholder: 'Filter symbols', 'aria-label': 'Filter symbols' });
  const sub = h('span', { class: 'panel-sub truncate' });
  const body = h('div', { class: 'panel-body' });
  sub.classList.replace('panel-sub', 'panel-title');
  mount(section, h('div', { class: 'panel-head' }, sub), h('div', { class: 'search-box' }, filter), body);
  let outline = null;
  let ctrl = null;

  async function load(path) {
    ctrl?.abort();
    sub.textContent = path ? basename(path) : 'Outline';
    if (!path) { outline = null; render(); return; }
    ctrl = new AbortController();
    try {
      outline = await api.outline(path, { signal: ctrl.signal });
      render();
    } catch (e) {
      if (!isAbort(e)) { outline = { symbols: [] }; render(); }
    }
  }

  function render() {
    if (!outline) { mount(body, h('div', { class: 'empty' }, icon('list-tree', 'xl'), h('h3', null, 'No file open'), h('p', null, 'The outline follows the active file.'))); return; }
    const q = filter.value.trim().toLowerCase();
    const syms = outline.symbols.filter((s) => !q || s.name.toLowerCase().includes(q));
    if (!syms.length) { mount(body, h('div', { class: 'empty' }, h('p', null, q ? 'No matching symbols' : 'No symbols in this file'))); return; }
    const cur = store.get('cursor');
    let currentIdx = -1;
    if (cur?.path === editor.active) syms.forEach((s, i) => { if (s.line <= cur.line) currentIdx = i; });
    mount(body, syms.map((s, i) => {
      const row = h('button', {
        class: `ol-row${i === currentIdx ? ' current' : ''}`,
        on: { click: () => { editor.activeView()?.gotoLine?.(s.line); editor.activeView()?.focus?.(); } },
      }, h('span', { class: `ol-kind k-${s.kind}` }, icon(KIND_ICON[s.kind] || 'dot', 'xs')), h('span', { class: 'ol-name' }, s.name), h('span', { class: 'ol-line num' }, String(s.line)));
      row.style.paddingLeft = `${12 + (q ? 0 : s.depth) * 14}px`;
      return row;
    }));
  }

  filter.addEventListener('input', render);
  store.subscribe('active', (p) => load(p), { now: true });
  let lastLine = 0;
  store.subscribe('cursor', (c) => { if (c && c.line !== lastLine) { lastLine = c.line; if (outline) render(); } });
  bus.on('ev:hl', (ev) => { if (ev?.path === editor.active) load(editor.active); });
  return { focus: () => filter.focus() };
}
