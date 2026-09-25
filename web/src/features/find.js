// Find in file (Mod+F): a floating bar over the active code view. Uses GET /file/find when the
// backend has `file.find` (B2a); until then it searches the raw text in the browser with the
// same semantics (literal | regex, smart case, whole word, UTF-16 ranges, 10k match cap).
import { h, mount } from '../core/dom.js';
import { api, has, isAbort } from '../core/api.js';
import { store } from '../core/store.js';
import { bus } from '../core/bus.js';
import { debounce, formatCount } from '../core/util.js';
import { icon } from '../ui/icons.js';

const LIMIT = 10000;

/** Build the matcher for a query; throws SyntaxError for an invalid regex. */
export function compileQuery(q, { mode = 'literal', caseMode = 'smart', word = false } = {}) {
  let src = mode === 'regex' ? q : q.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
  if (word) src = `(?<![\\p{L}\\p{N}_])(?:${src})(?![\\p{L}\\p{N}_])`;
  const insensitive = caseMode === 'insensitive' || (caseMode === 'smart' && q === q.toLowerCase());
  return new RegExp(src, `g${insensitive ? 'i' : ''}${word ? 'u' : ''}`);
}

/** Search text line by line. @returns {{total:number, truncated:boolean, matches:{line:number, ranges:number[][]}[]}} */
export function findInText(text, re, limit = LIMIT) {
  const matches = [];
  let total = 0;
  let truncated = false;
  let line = 0;
  let start = 0;
  const n = text.length;
  while (start <= n && !truncated) {
    let end = text.indexOf('\n', start);
    if (end < 0) end = n;
    line++;
    const s = text.charCodeAt(end - 1) === 13 ? text.slice(start, end - 1) : text.slice(start, end);
    re.lastIndex = 0;
    let ranges = null;
    for (let m = re.exec(s); m; m = re.exec(s)) {
      if (!m[0].length) { re.lastIndex++; if (re.lastIndex > s.length) break; continue; }
      (ranges ||= []).push([m.index, m.index + m[0].length]);
      if (++total >= limit) { truncated = true; break; }
    }
    if (ranges) matches.push({ line, ranges });
    if (end === n) break;
    start = end + 1;
  }
  return { total, truncated, matches };
}

const rawCache = new Map(); // path -> { mtime, text }
async function rawText(path, signal) {
  const meta = await api.file(path, { signal });
  const hit = rawCache.get(path);
  if (hit && hit.mtime === meta.mtimeMs) return hit.text;
  const max = store.get('meta')?.limits?.maxRawBytes || 32 * 1024 * 1024;
  if (meta.size > max) throw new Error('This file is too large to search in the browser.');
  const res = await fetch(api.rawUrl(path), { signal, credentials: 'same-origin' });
  if (!res.ok) throw new Error(`Could not read the file (${res.status})`);
  const text = await res.text();
  rawCache.clear(); // keep one file's text at most
  rawCache.set(path, { mtime: meta.mtimeMs, text });
  return text;
}

export function createFind({ editor, host }) {
  const input = h('input', { class: 'fb-input', type: 'text', placeholder: 'Find', 'aria-label': 'Find in file', spellcheck: 'false', autocomplete: 'off' });
  const count = h('span', { class: 'fb-count num', 'aria-live': 'polite' });
  const opt = (label, t, tip) => h('button', { class: 'icon-btn sm opt', 'aria-pressed': 'false', 'aria-label': tip, 'data-tip': tip }, h('span', { class: 'opt-t' }, t));
  const caseBtn = opt('case', 'Aa', 'Match case');
  const wordBtn = opt('word', 'ab', 'Whole word');
  const reBtn = opt('re', '.*', 'Regular expression');
  const prevBtn = h('button', { class: 'icon-btn sm', 'aria-label': 'Previous match', 'data-tip': 'Previous', 'data-keys': 'Shift+Enter', on: { click: () => step(-1) } }, icon('arrow-up', 'sm'));
  const nextBtn = h('button', { class: 'icon-btn sm', 'aria-label': 'Next match', 'data-tip': 'Next', 'data-keys': 'Enter', on: { click: () => step(1) } }, icon('arrow-down', 'sm'));
  const closeBtn = h('button', { class: 'icon-btn sm', 'aria-label': 'Close find', 'data-tip': 'Close', 'data-keys': 'Escape', on: { click: () => close() } }, icon('x', 'sm'));
  const bar = h('div', { class: 'findbar', role: 'search', hidden: true },
    icon('search', 'sm'), input, h('div', { class: 'fb-opts' }, caseBtn, wordBtn, reBtn), count, h('span', { class: 'fb-sep' }), prevBtn, nextBtn, closeBtn);
  host.appendChild(bar);

  let result = null; // { matches, total, truncated, flat: [{line, k, a}] }
  let idx = -1;
  let ctrl = null;
  let seq = 0;
  let forPath = null;

  for (const b of [caseBtn, wordBtn, reBtn]) b.addEventListener('click', () => { b.setAttribute('aria-pressed', String(b.getAttribute('aria-pressed') !== 'true')); run(); input.focus(); });

  const view = () => {
    const v = editor.activeView();
    if (v?.kind === 'markdown') return v.mode === 'source' ? v : null;
    return v?.kind === 'code' ? v : null;
  };
  const codeView = () => { const v = view(); return v?.kind === 'markdown' ? v.sourceView?.() : v; };

  function opts() {
    return {
      mode: reBtn.getAttribute('aria-pressed') === 'true' ? 'regex' : 'literal',
      caseMode: caseBtn.getAttribute('aria-pressed') === 'true' ? 'sensitive' : 'smart',
      word: wordBtn.getAttribute('aria-pressed') === 'true',
    };
  }

  async function run() {
    const v = codeView();
    const q = input.value;
    ctrl?.abort();
    const my = ++seq;
    bar.classList.remove('bad');
    if (!v || !q) { result = null; idx = -1; v?.setFind(null); count.textContent = ''; return; }
    ctrl = new AbortController();
    const o = opts();
    let res;
    try {
      if (has('file.find')) {
        res = await api.find(v.path, q, { mode: o.mode, case: o.caseMode, word: o.word ? 1 : 0, limit: LIMIT }, { signal: ctrl.signal });
      } else {
        let re;
        try { re = compileQuery(q, o); } catch { bar.classList.add('bad'); count.textContent = 'Invalid regex'; v.setFind(null); return; }
        const text = await rawText(v.path, ctrl.signal);
        if (my !== seq) return;
        res = findInText(text, re);
      }
    } catch (e) {
      if (isAbort(e) || my !== seq) return;
      count.textContent = e.message;
      bar.classList.add('bad');
      return;
    }
    if (my !== seq) return;
    forPath = v.path;
    const flat = [];
    for (const m of res.matches) m.ranges.forEach(([a], k) => flat.push({ line: m.line, k, a }));
    result = { ...res, flat, byLine: new Map(res.matches.map((m) => [m.line, m.ranges])) };
    // start at the first match at or after the caret
    const from = v.line || 1;
    idx = flat.findIndex((f) => f.line >= from);
    if (idx < 0) idx = flat.length ? 0 : -1;
    show({ scroll: true });
  }

  function show({ scroll = false } = {}) {
    const v = codeView();
    if (!v || !result) return;
    const f = result.flat[idx];
    v.setFind({ byLine: result.byLine, cur: f ? { line: f.line, k: f.k } : null });
    bar.classList.toggle('bad', !result.total);
    count.textContent = result.total
      ? `${formatCount(idx + 1)} of ${formatCount(result.total)}${result.truncated ? '+' : ''}`
      : 'No results';
    if (f && scroll) v.revealMatch(f.line, f.a);
  }

  function step(dir) {
    if (!result?.flat.length) { run(); return; }
    idx = (idx + dir + result.flat.length) % result.flat.length;
    show({ scroll: true });
  }

  function open(prefill) {
    const v = view();
    if (!v) return false;
    if (v.kind === 'markdown' && v.mode !== 'source') return false;
    bar.hidden = false;
    const sel = typeof prefill === 'string' ? prefill : window.getSelection()?.toString();
    if (sel && !sel.includes('\n') && sel.length < 200) input.value = sel;
    input.focus();
    input.select();
    if (input.value) run();
    return true;
  }

  function close({ focus = true } = {}) {
    if (bar.hidden) return;
    bar.hidden = true;
    ctrl?.abort();
    codeView()?.setFind(null);
    result = null;
    if (focus) editor.activeView()?.focus();
  }

  const runSoon = debounce(run, 90);
  input.addEventListener('input', runSoon);
  input.addEventListener('keydown', (e) => {
    if (e.key === 'Enter') { e.preventDefault(); runSoon.cancel?.(); if (!result || result.flat.length === 0) run(); else step(e.shiftKey ? -1 : 1); }
    else if (e.key === 'Escape') { e.preventDefault(); e.stopPropagation(); close(); }
    else if (e.key === 'F3') { e.preventDefault(); step(e.shiftKey ? -1 : 1); }
  });

  // Follow the active file: re-run the query there (or hide on non-code views).
  store.subscribe('active', () => {
    if (bar.hidden) return;
    const v = codeView();
    if (!v) { close({ focus: false }); return; }
    if (v.path !== forPath) run();
    else show();
  });
  bus.on('ev:fs', (ev) => { if (!bar.hidden && (ev?.changes || []).some((c) => c.path === forPath)) run(); });

  return {
    open,
    close,
    next: () => (bar.hidden ? open() : step(1)),
    prev: () => (bar.hidden ? open() : step(-1)),
    get isOpen() { return !bar.hidden; },
  };
}
