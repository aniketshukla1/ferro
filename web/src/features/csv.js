// CSV / TSV table view: RFC 4180 parsing (quotes, embedded newlines, CRLF),
// delimiter auto-detection, a virtualized grid with a sticky header, and a Table / Source toggle
// (Alt+M) like the Markdown preview. Loads on first use.
import { h, mount } from '../core/dom.js';
import { api, isAbort } from '../core/api.js';
import { store } from '../core/store.js';
import { bus } from '../core/bus.js';
import { formatCount } from '../core/util.js';
import { VirtualList } from '../core/virtual.js';
import { icon } from '../ui/icons.js';
import { createCodeView } from './viewer.js';

const MAX_TABLE_BYTES = 8 * 1024 * 1024;
const DELIMS = [',', '\t', ';', '|'];
const NAMES = { ',': 'comma', '\t': 'tab', ';': 'semicolon', '|': 'pipe' };

/** The delimiter whose per-line count is most consistent (and non-zero) over the first lines. */
export function detectDelimiter(text, path = '') {
  if (/\.tsv$/i.test(path)) return '\t';
  const lines = text.split(/\r?\n/, 21).filter((l) => l.length).slice(0, 20);
  let best = ',';
  let bestScore = -1;
  for (const d of DELIMS) {
    const counts = lines.map((l) => {
      let n = 0;
      let q = false;
      for (const c of l) {
        if (c === '"') q = !q;
        else if (c === d && !q) n++;
      }
      return n;
    });
    if (!counts.length || !counts[0]) continue;
    const same = counts.filter((n) => n === counts[0]).length;
    const score = same * 1000 + counts[0];
    if (score > bestScore) { bestScore = score; best = d; }
  }
  return best;
}

/**
 * RFC 4180 parse. Returns { rows: string[][], lines: number[] } where lines[i] is the 1-based
 * source line on which row i starts (quoted fields may span lines).
 */
export function parseCsv(text, delim) {
  const rows = [];
  const lines = [];
  let row = [];
  let field = '';
  let q = false;
  let line = 1;
  let rowLine = 1;
  for (let i = 0; i < text.length; i++) {
    const c = text[i];
    if (q) {
      if (c === '"') {
        if (text[i + 1] === '"') { field += '"'; i++; } else q = false;
      } else {
        if (c === '\n') line++;
        field += c;
      }
      continue;
    }
    if (c === '"' && field === '') q = true;
    else if (c === delim) { row.push(field); field = ''; }
    else if (c === '\n' || c === '\r') {
      if (c === '\r' && text[i + 1] === '\n') i++;
      row.push(field);
      rows.push(row);
      lines.push(rowLine);
      row = [];
      field = '';
      line++;
      rowLine = line;
    } else field += c;
  }
  if (field !== '' || row.length) { row.push(field); rows.push(row); lines.push(rowLine); }
  return { rows, lines };
}

export function createCsvView(path, meta, { preferSource = false } = {}) {
  const head = h('div', { class: 'csv-head', role: 'row' });
  const scroller = h('div', { class: 'csv-grid', tabindex: '0', role: 'grid', 'aria-label': `${path} table` });
  const body = h('div', { class: 'csv-body' }, head, scroller);
  const note = h('span', { class: 'csv-note faint small' });
  const el = h('div', { class: 'cv-wrap csv-wrap' }, body);
  let mode = preferSource || meta.size > MAX_TABLE_BYTES ? 'source' : 'table';
  let code = null;
  let data = null; // { rows, lines, cols, widths, delim }
  let vl = null;
  let ctrl = null;
  const listeners = new Set();

  const seg = h('div', { class: 'seg', role: 'group', 'aria-label': 'Table mode' },
    h('button', { 'aria-pressed': 'true', 'data-mode': 'table', 'data-tip': 'Table', 'data-keys': 'Alt+M', on: { click: () => setMode('table') } }, 'Table'),
    h('button', { 'aria-pressed': 'false', 'data-mode': 'source', 'data-tip': 'Source', 'data-keys': 'Alt+M', on: { click: () => setMode('source') } }, 'Source'));
  const syncSeg = () => { for (const b of seg.children) b.setAttribute('aria-pressed', String(b.dataset.mode === mode)); };
  const bar = h('span', { class: 'csv-bar' }, note, seg);

  async function load() {
    ctrl?.abort();
    ctrl = new AbortController();
    try {
      const res = await fetch(api.rawUrl(path), { signal: ctrl.signal, credentials: 'same-origin' });
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      const text = (await res.text()).replace(/^﻿/, '');
      const delim = detectDelimiter(text, path);
      const { rows, lines } = parseCsv(text, delim);
      const cols = rows.reduce((m, r) => Math.max(m, r.length), 0);
      // Column widths from the first 200 rows: ~8.2 px per monospace character plus padding, clamped.
      const widths = Array.from({ length: cols }, (_, c) => {
        let n = 3;
        for (let r = 0; r < Math.min(rows.length, 200); r++) n = Math.max(n, (rows[r][c] || '').length);
        return Math.min(320, Math.max(64, Math.round(n * 8.2) + 28));
      });
      data = { rows, lines, cols, widths, delim };
      render();
    } catch (e) {
      if (isAbort(e)) return;
      mount(scroller, h('div', { class: 'empty' }, icon('alert', 'xl'), h('h3', null, 'Cannot show this file as a table'), h('p', null, e.message),
        h('button', { class: 'btn sm', on: { click: () => setMode('source') } }, 'Show source')));
    }
  }

  function render() {
    const { rows, cols, widths, delim } = data;
    const template = `56px ${widths.map((w) => `${w}px`).join(' ')}`;
    el.style.setProperty('--csv-cols', template);
    const header = rows[0] || [];
    mount(head, h('span', { class: 'csv-rn', role: 'columnheader' }, '#'),
      Array.from({ length: cols }, (_, c) => h('span', { class: 'csv-th', role: 'columnheader', title: header[c] || '' }, header[c] || '')));
    note.textContent = `${formatCount(Math.max(0, rows.length - 1))} rows × ${cols} columns · ${NAMES[delim]}`;
    vl?.destroy();
    mount(scroller);
    vl = new VirtualList({
      scroller,
      rowHeight: 26,
      overscan: 12,
      create: () => h('div', { class: 'csv-row', role: 'row' }),
      update: (box, i) => {
        const r = rows[i + 1] || [];
        mount(box, h('span', { class: 'csv-rn num', role: 'rowheader' }, String(i + 1)),
          Array.from({ length: cols }, (_, c) => h('span', { class: 'csv-td', role: 'gridcell', title: (r[c] || '').length > 30 ? r[c] : null }, r[c] ?? '')));
      },
    });
    vl.setCount(Math.max(0, rows.length - 1));
  }

  // Header and grid scroll horizontally together.
  scroller.addEventListener('scroll', () => { head.scrollLeft = scroller.scrollLeft; }, { passive: true });
  // Double-click a row: its source line.
  scroller.addEventListener('dblclick', (e) => {
    const box = e.target.closest('.csv-row');
    if (box && box.__index >= 0 && data) { setMode('source'); code.gotoLine(data.lines[box.__index + 1] || 1); }
  });

  function ensureCode() {
    if (!code) {
      code = createCodeView(path, meta);
      code.el.hidden = true;
      el.appendChild(code.el);
    }
    return code;
  }

  function setMode(next) {
    if (next === 'table' && meta.size > MAX_TABLE_BYTES) next = 'source';
    if (next === mode && (next === 'table' || code)) { syncSeg(); return; }
    mode = next;
    syncSeg();
    if (mode === 'source') {
      const c = ensureCode();
      body.hidden = true;
      c.el.hidden = false;
      c.onShow();
      c.focus();
    } else {
      if (code) code.el.hidden = true;
      body.hidden = false;
      if (!data) load();
      scroller.focus({ preventScroll: true });
      emitCursor();
    }
    bus.emit('view:state', path);
    for (const fn of listeners) fn(mode);
  }

  function emitCursor() {
    store.set('cursor', { path, preview: true, language: /\.tsv$/i.test(path) ? 'TSV' : 'CSV', total: meta.lines, line: 1 });
  }

  const offFs = bus.on('ev:fs', (ev) => {
    if ((ev?.changes || []).some((c) => c.path === path) || ev?.overflow) { data = null; if (mode === 'table') load(); }
  });

  if (mode === 'table') load();
  else setMode('source');
  syncSeg();

  return {
    el,
    kind: 'table',
    get mode() { return mode; },
    get total() { return meta.lines; },
    get line() { return mode === 'source' ? code?.line : 1; },
    toolbar() { return bar; },
    sourceView() { return mode === 'source' ? code : null; },
    toggleMode() { setMode(mode === 'table' ? 'source' : 'table'); },
    onModeChange(fn) { listeners.add(fn); return () => listeners.delete(fn); },
    selection() { return mode === 'source' ? code.selection() : { path, start: 1, end: 1 }; },
    focus() { (mode === 'source' ? code : null)?.focus() ?? scroller.focus({ preventScroll: true }); },
    gotoLine(n, o = {}) { setMode('source'); code.gotoLine(n, o); },
    selectLines(a, b) { setMode('source'); code.selectLines(a, b); },
    state() { return { mode, scrollTop: scroller.scrollTop, source: code?.state() }; },
    restore(s) {
      if (s?.mode === 'source') { setMode('source'); if (s.source) code.restore(s.source); }
    },
    onShow() {
      if (mode === 'source') { code.onShow(); return; }
      vl?.schedule(true);
      emitCursor();
    },
    destroy() {
      ctrl?.abort();
      offFs?.();
      vl?.destroy();
      code?.destroy();
      listeners.clear();
    },
  };
}
