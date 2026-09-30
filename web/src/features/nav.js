// Code navigation (FRONTEND.md § 6.15, API.md § 4.8): go to definition, the references panel
// in the inspector, and the hover card. Loads on first use; the code viewer only reports
// positions (bus `nav:definition` / `nav:hover`, view.position()).
import { h, mount, setTrustedHTML, textRange } from '../core/dom.js';
import { request, has, isAbort } from '../core/api.js';
import { basename, dirname, formatMs, isMac, plural } from '../core/util.js';
import { VirtualList } from '../core/virtual.js';
import { keysEl } from '../core/keys.js';
import { icon, fileIcon } from '../ui/icons.js';
import { toast } from '../ui/overlay.js';

export const KIND_ICON = { function: 'zap', method: 'zap', class: 'braces', struct: 'braces', enum: 'list-tree', interface: 'braces', trait: 'braces', type: 'braces', module: 'layers', const: 'hash', var: 'hash', field: 'hash', impl: 'layers', macro: 'sparkles', heading: 'hash', other: 'dot' };
const IDENT = /^[\p{L}\p{N}_$]+/u;
const q = (pos) => ({ path: pos.path, line: pos.line, col: pos.col });

// ---------- go to definition ----------

/**
 * F12 / Mod+click. One target opens it; several open a picker; none says so.
 * @param {{path:string, line:number, col:number}} pos
 * @param {{onOpen:Function, pick:(title:string, items:object[]) => void, openRefs:Function}} ctx
 */
export async function goToDefinition(pos, ctx) {
  hideHover();
  let res;
  try {
    res = await request('nav/definition', { query: q(pos), label: 'definition' });
  } catch (e) {
    if (e.status === 404) toast({ title: 'No definition found', timeout: 1800 });
    else toast({ kind: 'error', title: 'Go to definition failed', message: e.message });
    return;
  }
  const defs = res?.definitions || [];
  if (!defs.length) { toast({ title: 'No definition found', timeout: 1800 }); return; }
  // Already on the only definition: the useful next step is its references (as in most editors).
  if (defs.length === 1 && defs[0].path === pos.path && defs[0].line === pos.line) {
    findReferences(pos, ctx);
    return;
  }
  if (defs.length === 1) { openTarget(defs[0], ctx); return; }
  ctx.pick(`${plural(defs.length, 'definition')} of ${defs[0].name}`, defs.map((d) => ({
    key: `def:${d.path}:${d.line}`,
    icon: icon(KIND_ICON[d.kind] || 'dot', 'sm'),
    label: d.name,
    desc: `${d.path}:${d.line}`,
    right: h('span', { class: 'faint small' }, d.kind),
    text: `${d.name} ${d.path}`,
    run: () => openTarget(d, ctx),
  })));
}

function openTarget(t, ctx) {
  ctx.onOpen(t.path, { line: t.line, focus: true, preview: false });
}

// ---------- hover card ----------

let card = null; // { el, key, x, y }
let hoverSeq = 0;
let hoverCtrl = null;
let hideTimer = 0;

/** Show the hover card for a position (bus `nav:hover`); null hides it. */
export async function showHover(ev, ctx) {
  if (!ev) { hideHover(); return; }
  const key = `${ev.path}:${ev.line}:${ev.col}`;
  if (card?.key === key) return;
  const my = ++hoverSeq;
  hoverCtrl?.abort();
  hoverCtrl = new AbortController();
  let info;
  try {
    info = await request('nav/hover', { query: q(ev), signal: hoverCtrl.signal });
  } catch (e) {
    if (!isAbort(e) && my === hoverSeq) hideHover();
    return;
  }
  if (my !== hoverSeq || !info?.name) return;
  renderCard(info, ev, key, ctx);
  if (info.signature && has('hl.classes')) {
    request('highlight', { method: 'POST', body: { code: info.signature, path: info.path }, signal: hoverCtrl.signal })
      .then((r) => {
        const pre = card?.key === key && card.el.querySelector('.hov-sig');
        if (pre && r?.lines?.length) setTrustedHTML(pre, r.lines.join('\n'), 'hl');
      })
      .catch(() => {});
  }
}

function renderCard(info, ev, key, ctx) {
  hideHover();
  const act = (label, keys, run) => h('button', { class: 'btn ghost sm', on: { click: () => { hideHover(); run(); } } }, label, keysEl(keys));
  const el = h('div', { class: 'hov-card', role: 'dialog', 'aria-label': `${info.kind} ${info.name}` },
    h('div', { class: 'hov-head' },
      icon(KIND_ICON[info.kind] || 'dot', 'sm'),
      h('span', { class: 'hov-name' }, info.name),
      h('span', { class: 'hov-kind faint' }, info.kind),
      h('button', { class: 'hov-loc truncate', title: `${info.path}:${info.line}`, on: { click: () => { hideHover(); openTarget(info, ctx); } } }, `${basename(info.path)}:${info.line}`)),
    info.signature ? h('pre', { class: 'hov-sig' }, info.signature) : null,
    info.doc ? h('div', { class: 'hov-doc' }, info.doc) : null,
    h('div', { class: 'hov-actions' },
      act('Go to definition', 'F12', () => goToDefinition(ev, ctx)),
      act('Find references', 'Shift+F12', () => findReferences(ev, ctx))));
  document.body.appendChild(el);
  // Below the pointer, flipped above when it would leave the viewport; clamped horizontally.
  const r = el.getBoundingClientRect();
  const top = ev.y + 18 + r.height > innerHeight - 8 ? Math.max(8, ev.y - 12 - r.height) : ev.y + 18;
  el.style.left = `${Math.max(8, Math.min(ev.x - 16, innerWidth - r.width - 8))}px`;
  el.style.top = `${top}px`;
  card = { el, key, x: ev.x, y: ev.y };
  document.addEventListener('mousemove', onMove, true);
  document.addEventListener('mousedown', onDown, true);
  document.addEventListener('keydown', onKey, true);
  document.addEventListener('scroll', hideHover, true);
}

function onMove(e) {
  if (!card) return;
  if (card.el.contains(e.target) || Math.hypot(e.clientX - card.x, e.clientY - card.y) < 24) {
    clearTimeout(hideTimer);
    hideTimer = 0;
  } else if (!hideTimer) hideTimer = setTimeout(hideHover, 250);
}
function onDown(e) { if (card && !card.el.contains(e.target)) hideHover(); }
function onKey(e) { if (e.key === 'Escape' && card) { e.stopPropagation(); hideHover(); } }

export function hideHover() {
  hoverSeq++;
  hoverCtrl?.abort();
  clearTimeout(hideTimer);
  hideTimer = 0;
  if (!card) return;
  card.el.remove();
  card = null;
  document.removeEventListener('mousemove', onMove, true);
  document.removeEventListener('mousedown', onDown, true);
  document.removeEventListener('keydown', onKey, true);
  document.removeEventListener('scroll', hideHover, true);
}

// ---------- references (inspector tab) ----------

const state = { pos: null, name: '', loading: false, error: '', refs: [], truncated: false, ms: 0 };
let panel = null;
let refSeq = 0;

/** Shift+F12: fill the References tab and show it. */
export async function findReferences(pos, ctx) {
  hideHover();
  const my = ++refSeq;
  Object.assign(state, { pos, name: '', loading: true, error: '', refs: [], truncated: false });
  ctx.openRefs();
  panel?.update();
  try {
    const res = await request('nav/references', { query: { ...q(pos), limit: 1000 }, label: 'references' });
    if (my !== refSeq) return;
    Object.assign(state, { loading: false, refs: res?.references || [], truncated: !!res?.truncated, ms: res?.ms || 0 });
  } catch (e) {
    if (my !== refSeq) return;
    Object.assign(state, { loading: false, error: e.status === 404 ? 'No symbol at this position.' : e.message });
  }
  panel?.update();
}

/** Inspector tab body; renders the latest findReferences() state. */
export function renderRefsTab(el, ctx) {
  const summary = h('div', { class: 'refs-summary faint small', role: 'status', 'aria-live': 'polite' });
  const refresh = h('button', { class: 'icon-btn sm', 'aria-label': 'Refresh references', 'data-tip': 'Refresh', on: { click: () => state.pos && findReferences(state.pos, ctx) } }, icon('refresh', 'sm'));
  const scroller = h('div', { class: 'refs-list', tabindex: '0', role: 'listbox', 'aria-label': 'References' });
  const empty = h('div', { class: 'empty refs-empty' });
  mount(el, h('div', { class: 'refs' }, h('div', { class: 'refs-head' }, h('span', { class: 'refs-title' }, 'References'), refresh), summary, empty, scroller));

  let rows = []; // { type: 'file', path, count } | { type: 'ref', path, line, col }
  let active = -1;
  const previews = new Map(); // `${path}#${chunk}` -> Map<line, {html, cut}> | 'loading'
  const CHUNK = 200;

  const vl = new VirtualList({
    scroller,
    rowHeight: 24,
    overscan: 8,
    create: () => h('div', { class: 'refs-row', role: 'option' }),
    update: (box, i) => paintRow(box, rows[i], i),
    onRange: (a, b) => loadPreviews(a, b),
    onPaint: () => markIdents(),
  });

  function paintRow(box, r, i) {
    if (!r) return;
    box.setAttribute('aria-selected', String(i === active));
    box.id = `ref-${i}`;
    if (r.type === 'file') {
      box.className = 'refs-row refs-file';
      mount(box, fileIcon(r.path), h('span', { class: 'lr-name' }, basename(r.path)), h('span', { class: 'lr-dir' }, dirname(r.path)), h('span', { class: 'count' }, String(r.count)));
      box.title = r.path;
      return;
    }
    box.className = `refs-row refs-ref${i === active ? ' active' : ''}`;
    box.title = `${r.path}:${r.line}`;
    const code = h('span', { class: 'refs-code' });
    const p = previewFor(r);
    if (p) {
      setTrustedHTML(code, p.html ?? '', 'hl');
      code.__shift = trimLeading(code);
    }
    code.__col = r.col;
    mount(box, h('span', { class: 'refs-ln num' }, String(r.line)), code);
  }

  function previewFor(r) {
    const c = previews.get(`${r.path}#${Math.floor((r.line - 1) / CHUNK)}`);
    return c && c !== 'loading' ? c.get(r.line) : null;
  }

  async function loadPreviews(a, b) {
    const want = new Map();
    for (let i = Math.max(0, a); i <= b && i < rows.length; i++) {
      const r = rows[i];
      if (r?.type !== 'ref') continue;
      const k = `${r.path}#${Math.floor((r.line - 1) / CHUNK)}`;
      if (!previews.has(k)) want.set(k, r);
    }
    for (const [k, r] of want) {
      previews.set(k, 'loading');
      const chunk = Math.floor((r.line - 1) / CHUNK);
      request('file/lines', { query: { path: r.path, from: chunk * CHUNK + 1, count: CHUNK, hl: 1, maxCols: 400 } })
        .then((res) => {
          previews.set(k, new Map((res?.lines || []).map((l) => [l.n, l])));
          vl.refresh();
        })
        .catch(() => previews.set(k, new Map()));
    }
  }

  // Previews drop their indentation; __shift keeps the reference column pointing at the name.
  function trimLeading(code) {
    let cut = 0;
    const walker = document.createTreeWalker(code, NodeFilter.SHOW_TEXT);
    for (let n = walker.nextNode(); n; n = walker.nextNode()) {
      const m = /^\s*/.exec(n.data)[0].length;
      cut += m;
      n.data = n.data.slice(m);
      if (n.data.length) break;
    }
    return cut;
  }

  function markIdents() {
    if (typeof Highlight !== 'function' || !globalThis.CSS?.highlights) return;
    const ranges = [];
    for (const box of vl.mounted.values()) {
      const code = box.querySelector('.refs-code');
      if (!code?.__col) continue;
      const start = code.__col - 1 - (code.__shift || 0);
      const len = IDENT.exec(code.textContent.slice(Math.max(0, start)))?.[0].length || 0;
      const range = len && start >= 0 ? textRange(code, start, start + len) : null;
      if (range) ranges.push(range);
    }
    CSS.highlights.set('ferro-ref', new Highlight(...ranges));
  }

  function open(i, focus) {
    const r = rows[i];
    if (!r) return;
    if (r.type === 'file') { ctx.onOpen(r.path, { preview: true, focus }); return; }
    ctx.onOpen(r.path, { line: r.line, preview: !focus, focus });
  }

  function setActive(i) {
    active = Math.max(0, Math.min(rows.length - 1, i));
    scroller.setAttribute('aria-activedescendant', `ref-${active}`);
    vl.scrollToIndex(active);
    vl.refresh();
  }

  scroller.addEventListener('click', (e) => {
    const box = e.target.closest('.refs-row');
    if (!box || !(box.__index >= 0)) return;
    active = box.__index;
    vl.refresh();
    open(active, false);
  });
  scroller.addEventListener('dblclick', (e) => {
    const box = e.target.closest('.refs-row');
    if (box && box.__index >= 0) open(box.__index, true);
  });
  scroller.addEventListener('keydown', (e) => {
    if (!rows.length) return;
    if (e.key === 'ArrowDown') setActive(active + 1);
    else if (e.key === 'ArrowUp') setActive(active - 1);
    else if (e.key === 'Home') setActive(0);
    else if (e.key === 'End') setActive(rows.length - 1);
    else if (e.key === 'Enter') open(active < 0 ? 0 : active, true);
    else if (e.key === ' ') open(active < 0 ? 0 : active, false);
    else return;
    e.preventDefault();
  });

  function update() {
    const s = state;
    previews.clear(); // files may have changed since the last query
    if (!s.pos) {
      mount(empty,
        h('p', null, 'Put the caret on a name and press ', keysEl('Shift+F12'), `, or ${isMac ? '⌘' : 'Ctrl'}+click a name to go to its definition.`),
        has('nav') ? null : h('p', { class: 'faint small' }, 'Code navigation needs a ferro server with the nav feature.'));
      empty.hidden = false;
      summary.textContent = '';
      rows = [];
      vl.setCount(0);
      return;
    }
    const byFile = new Map();
    for (const r of s.refs) {
      if (!byFile.has(r.path)) byFile.set(r.path, []);
      byFile.get(r.path).push(r);
    }
    rows = [];
    for (const [path, list] of byFile) {
      rows.push({ type: 'file', path, count: list.length });
      for (const r of list) rows.push({ type: 'ref', path, line: r.line, col: r.col || 1 });
    }
    active = -1;
    scroller.removeAttribute('aria-activedescendant');
    if (s.loading) summary.replaceChildren(h('span', { class: 'spinner' }), ' Finding references…');
    else if (s.error) summary.textContent = s.error;
    else summary.textContent = `${plural(s.refs.length, 'reference')} in ${plural(byFile.size, 'file')} · ${formatMs(s.ms)}${s.truncated ? ' · truncated' : ''}`;
    empty.hidden = s.loading || !!s.refs.length;
    if (!empty.hidden) mount(empty, h('p', null, s.error || 'No references found.'));
    vl.setCount(rows.length);
    scroller.scrollTop = 0;
  }

  panel = { update };
  update();
  return panel;
}
