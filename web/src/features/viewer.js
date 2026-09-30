// Code viewer: virtualized rows fetched in 500-line chunks from /file/lines (hl=1),
// sticky gutter, caret line, line-range selection (gutter drag / Shift+click / Shift+arrows),
// jump-to-line with a flash, exact-highlight refresh on `hl` events.
import { h, mount, setTrustedHTML, textRange } from '../core/dom.js';
import { api, has, isAbort, request } from '../core/api.js';
import { VirtualList } from '../core/virtual.js';
import { store } from '../core/store.js';
import { bus } from '../core/bus.js';
import { LRU, formatCount } from '../core/util.js';
import { icon } from '../ui/icons.js';

const CHUNK = 500;

let charWidth = 0;
let charWidthFor = '';
function measureCharWidth(host) {
  const fs = getComputedStyle(document.documentElement).getPropertyValue('--code-fs');
  if (charWidth && charWidthFor === fs) return charWidth;
  charWidthFor = fs;
  const probe = h('span', { class: 'cv' }, '0000000000');
  probe.style.position = 'absolute';
  probe.style.visibility = 'hidden';
  probe.style.contain = 'none';
  probe.style.inset = 'auto';
  probe.style.width = 'auto';
  probe.style.height = 'auto';
  host.appendChild(probe);
  charWidth = probe.getBoundingClientRect().width / 10 || 7.8;
  probe.remove();
  return charWidth;
}

function lineHeight() {
  const v = getComputedStyle(document.documentElement).getPropertyValue('--code-lh');
  return parseFloat(v) || 20;
}

/**
 * @param {string} path
 * @param {{lines:number, language?:string}} meta
 */
export function createCodeView(path, meta) {
  const scroller = h('div', { class: 'cv', tabindex: '0', role: 'region', 'aria-label': `${path}, ${formatCount(meta.lines)} lines` });
  const sizer = h('div', { class: 'cv-sizer' });
  scroller.appendChild(sizer);
  const shadow = h('div', { class: 'cv-shadow' });
  const el = h('div', { class: 'cv-wrap' }, shadow, scroller);

  const chunks = new LRU(24);
  const pending = new Map();
  let total = meta.lines;
  let cur = 1;
  let anchor = 1;
  let selA = 0;
  let selB = 0;
  let maxW = 0;
  let flash = 0;
  let diags = null; // line -> { severity, message } from language servers (problems.js)
  let restoreTop = null;
  let destroyed = false;
  const lh = lineHeight();

  const rulerCanvas = h('canvas', { class: 'cv-ruler', width: 14, height: 100 });
  el.appendChild(rulerCanvas);

  rulerCanvas.addEventListener('click', (e) => {
    const rect = rulerCanvas.getBoundingClientRect();
    const ratio = Math.max(0, Math.min(1, (e.clientY - rect.top) / rect.height));
    const targetLine = Math.max(1, Math.min(total, Math.round(ratio * total)));
    gotoLine(targetLine);
  });

  function gotoLine(n, { select = false, flashIt = true } = {}) {
    n = Math.max(1, Math.min(total || 1, n));
    cur = n;
    anchor = n;
    selA = select ? n : 0;
    selB = select ? n : 0;
    flash = flashIt ? n : 0;
    vl.scrollToIndex(n - 1, 'center');
    vl.refresh();
    emitCursor();
    if (flashIt) setTimeout(() => { if (flash === n) { flash = 0; vl.refresh(); } }, 900);
  }

  function paintRuler() {
    if (!rulerCanvas.offsetParent || !total) return; // hidden (ui.overviewRuler: false) or off-screen
    const h = rulerCanvas.clientHeight || scroller.clientHeight || 400;
    if (rulerCanvas.height !== h) rulerCanvas.height = h;
    const ctx = rulerCanvas.getContext('2d');
    ctx.clearRect(0, 0, 14, h);

    if (find) {
      ctx.fillStyle = getComputedStyle(scroller).getPropertyValue('--warn') || '#f59e0b';
      for (const line of find.byLine.keys()) {
        const y = Math.round(((line - 1) / total) * h);
        ctx.fillRect(1, Math.max(0, y - 1), 12, 3);
      }
    }

    if (occurrencesWord && occurrencesLines.size) {
      ctx.fillStyle = 'rgba(150, 150, 150, 0.6)';
      for (const line of occurrencesLines) {
        const y = Math.round(((line - 1) / total) * h);
        ctx.fillRect(3, Math.max(0, y - 1), 8, 2);
      }
    }
  }

  // ---------- decorations (find + occurrences) ----------
  /** @type {{byLine: Map<number, number[][]>, cur: {line:number, k:number}|null}|null} */
  let find = null;
  let occurrencesWord = '';
  const occurrencesLines = new Set();
  const canHighlight = typeof Highlight === 'function' && !!globalThis.CSS?.highlights;

  function paintDecorations() {
    zone?.place();
    paintFind();
    paintOccurrences();
    paintRuler();
  }

  // A box under lines start..end (inline-edit.js): the last row grows by its height h.
  let zone = null;
  let zoneModel = null;
  const zoneExtra = (i) => (zone && i === zone.end - 1 ? zone.h : 0);
  async function fitZone() {
    if (wrap) wrapper?.relayout();
    else if (!zone) { zoneModel = null; if (vl.hm) vl.setHeights(null); } else {
      zoneModel ||= (await import('../core/heights.js')).heightModel((i) => lh + zoneExtra(i));
      if (zone && !wrap) vl.setHeights(zoneModel);
    }
    zone?.place();
  }
  const inZone = (e) => !!e.target?.closest?.('.cv-zone, .cv-selbar');
  const selected = () => bus.emit('view:select', path);

  function paintFind() {
    if (!canHighlight || !find || !el.isConnected || el.closest('[hidden]')) return;
    const all = [];
    const current = [];
    for (const [i, row] of vl.mounted) {
      const n = i + 1;
      const ranges = find.byLine.get(n);
      if (!ranges || row.classList.contains('skel')) continue;
      ranges.forEach(([a, b], k) => {
        const r = textRange(row.lastChild, a, b);
        if (r) (find.cur && find.cur.line === n && find.cur.k === k ? current : all).push(r);
      });
    }
    CSS.highlights.set('ferro-find', new Highlight(...all));
    CSS.highlights.set('ferro-find-current', new Highlight(...current));
  }

  function clearFind() {
    find = null;
    if (canHighlight) { CSS.highlights.delete('ferro-find'); CSS.highlights.delete('ferro-find-current'); }
    paintRuler();
  }

  function clearOccurrences() {
    occurrencesWord = '';
    occurrencesLines.clear();
    if (canHighlight) CSS.highlights.delete('ferro-occ');
    paintRuler();
  }

  const wordRe = (w, f = '') => new RegExp(`\\b${w.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}\\b`, f);
  function setOccurrences(word) {
    if (!word || word.length < 2) { clearOccurrences(); return; }
    occurrencesWord = word;
    occurrencesLines.clear();
    const re = wordRe(occurrencesWord);
    for (const [chunkIdx, c] of chunks.map) {
      if (!c.lines) continue;
      c.lines.forEach((l, idx) => {
        const text = l.text ?? (l.html ? l.html.replace(/<[^>]+>|&[#\w]+;/g, ' ') : '');
        if (text && re.test(text)) occurrencesLines.add(chunkIdx * CHUNK + idx + 1);
      });
    }
    paintOccurrences();
    paintRuler();
  }

  function paintOccurrences() {
    if (!canHighlight || !occurrencesWord || !el.isConnected) return;
    const all = [];
    const re = wordRe(occurrencesWord, 'g');
    for (const [i, row] of vl.mounted) {
      if (!lineAt(i + 1) || row.classList.contains('skel')) continue;
      const code = row.lastChild, cut = code.querySelector?.('.cv-cut');
      const text = cut ? code.textContent.slice(0, -cut.textContent.length) : code.textContent;
      re.lastIndex = 0;
      let m;
      while ((m = re.exec(text)) !== null) {
        const r = textRange(code, m.index, m.index + m[0].length);
        if (r) all.push(r);
      }
    }
    CSS.highlights.set('ferro-occ', new Highlight(...all));
  }

  // Word wrap: measured row heights, in features/wrap.js (loaded on first use).
  let wrap = false;
  let wrapper = null;
  async function toggleWrap(silent = false) {
    if (!wrap && total > 50000) {
      if (!silent) bus.emit('toast', { kind: 'info', title: 'Word wrap disabled', message: 'Word wrap is disabled for files over 50,000 lines' });
      return;
    }
    wrap = !wrap;
    scroller.classList.toggle('wrapped', wrap);
    wrapper?.destroy();
    wrapper = null;
    if (wrap) {
      maxW = 0;
      sizer.style.width = '';
      const { wrapView } = await import('./wrap.js');
      if (wrap && !destroyed) wrapper = wrapView({ vl, scroller, lh, lineAt, charWidth: () => measureCharWidth(el), extra: zoneExtra });
    } else requestAnimationFrame(measureWidth);
    if (zone) { zoneModel = null; await fitZone(); }
    vl.refresh();
  }

  const vl = new VirtualList({
    scroller, sizer, rowHeight: lh, overscan: 24,
    create: createRow, update: updateRow, onRange: ensureRange, onPaint: paintDecorations,
  });
  scroller.__vl = vl;

  requestAnimationFrame(() => {
    const cw = measureCharWidth(el);
    const digits = Math.max(3, String(total).length);
    scroller.style.setProperty('--gutter-w', `${Math.ceil(digits * cw + 30)}px`);
  });
  vl.setCount(total);

  function createRow() {
    const row = h('div', { class: 'cv-row' }, h('span', { class: 'cv-ln' }), h('span', { class: 'cv-code' }));
    return row;
  }

  function lineAt(n) {
    const idx = Math.floor((n - 1) / CHUNK);
    const c = chunks.map.get(idx);
    return c ? c.lines[n - 1 - idx * CHUNK] : undefined;
  }

  function updateRow(row, i) {
    const n = i + 1;
    const ln = row.firstChild;
    const code = row.lastChild;
    ln.textContent = String(n);
    let cls = 'cv-row';
    if (n === cur) cls += ' cur';
    if (selA && n >= selA && n <= selB) cls += ' sel';
    if (n === flash) cls += ' flash';
    if (zone && n >= zone.start && n <= zone.end) cls += ' editing';
    const dg = diags?.get(n);
    if (dg) cls += ` diag-${dg.severity}`;
    if (dg || ln.title) ln.title = dg ? dg.message : '';
    const line = lineAt(n);
    if (line) {
      if (row.__html !== line.html || row.__cut !== line.cut) {
        setTrustedHTML(code, line.html ?? '', 'hl');
        if (line.cut) {
          const cutBtn = h('button', {
            class: 'cv-cut btn sm',
            'aria-label': 'Show full line',
            on: {
              click: async (e) => {
                e.stopPropagation();
                try {
                  const res = await api.lines(path, n, 1, { maxCols: 1000000 });
                  if (res.lines?.[0]) {
                    line.html = res.lines[0].html;
                    delete line.cut;
                    delete line.__cols;
                    wrapper?.relayout();
                    vl.refresh((idx) => idx === i);
                  }
                } catch {}
              },
            },
          }, `… (${formatCount(line.cut)} more) · Show full line`);
          code.appendChild(cutBtn);
        }
        row.__html = line.html;
        row.__cut = line.cut;
      }
    } else {
      cls += ' skel';
      if (row.__html !== null) {
        const bar = h('span', { class: 'skel' });
        bar.style.width = `${80 + ((n * 97) % 360)}px`;
        mount(code, bar);
        row.__html = null;
      }
    }
    row.className = cls;
    row.dataset.n = String(n);
  }

  // ---------- data ----------
  function ensureRange(first, last) {
    const lo = Math.floor(first / CHUNK);
    const hi = Math.floor(last / CHUNK);
    for (const [idx, ctrl] of pending) {
      if (idx < lo - 1 || idx > hi + 1) { ctrl.abort(); pending.delete(idx); }
    }
    for (let idx = lo; idx <= hi; idx++) fetchChunk(idx);
    // prefetch the next chunk in the scroll direction
    if ((hi + 1) * CHUNK < total) fetchChunk(hi + 1);
  }

  async function fetchChunk(idx) {
    if (chunks.has(idx) || pending.has(idx) || idx * CHUNK >= Math.max(total, 1)) return;
    const ctrl = new AbortController();
    pending.set(idx, ctrl);
    try {
      const res = await api.lines(path, idx * CHUNK + 1, CHUNK, { signal: ctrl.signal });
      if (destroyed) return;
      // Rows are one line each: drop a line terminator the highlighter may leave inside the last span.
      for (const l of res.lines) if (l.html && /[\r\n]/.test(l.html)) l.html = l.html.replace(/\r?\n(?=(<\/span>)*$)/, '');
      chunks.set(idx, { lines: res.lines, exact: res.exact });
      if (res.total !== total) {
        total = res.total;
        vl.setCount(total);
      }
      wrapper?.relayout(); // these lines' wrapped heights are known now
      vl.refresh((i) => Math.floor(i / CHUNK) === idx);
      if (restoreTop != null && idx === Math.floor(vl.visibleRange().first / CHUNK)) {
        scroller.scrollTop = restoreTop;
        restoreTop = null;
      }
      requestAnimationFrame(measureWidth);
    } catch (e) {
      if (!isAbort(e)) console.warn('[viewer] chunk', idx, e);
    } finally {
      pending.delete(idx);
    }
  }

  function measureWidth() {
    if (wrap) return;
    let w = maxW;
    for (const row of vl.mounted.values()) if (!row.classList.contains('skel')) w = Math.max(w, row.scrollWidth);
    if (w > maxW + 1) {
      maxW = w;
      sizer.style.width = `${Math.ceil(maxW)}px`;
    }
  }

  // ---------- cursor & selection ----------
  function emitCursor() {
    store.set('cursor', { path, line: cur, selStart: selA || cur, selEnd: selB || cur, total, language: meta.language });
    bus.emit('view:state', path);
  }

  function setCursor(n, { extend = false, reveal = true, keepSel = false } = {}) {
    n = Math.max(1, Math.min(total || 1, n));
    if (extend) {
      selA = Math.min(anchor, n);
      selB = Math.max(anchor, n);
    } else if (!keepSel) {
      anchor = n;
      selA = 0;
      selB = 0;
    }
    cur = n;
    if (reveal) vl.scrollToIndex(n - 1);
    vl.refresh();
    emitCursor();
  }

  function lineFromClientY(y) {
    const rect = scroller.getBoundingClientRect();
    const vTop = vl.virtualTop(scroller.scrollTop, scroller.clientHeight);
    return vl.indexAt(vTop + (y - rect.top) - vl.topPad) + 1;
  }

  // ---------- navigation hooks (F5): positions only; features/nav.js does the rest ----------
  let curCol = 1;
  /** {line, col} (1-based UTF-16 column) of a DOM text position inside a code row, or null. */
  function posOf(node, off) {
    const code = node?.nodeType === 3 ? node.parentElement?.closest('.cv-code') : node?.closest?.('.cv-code');
    const row = code?.parentElement;
    if (!row || !(row.__index >= 0) || !scroller.contains(row)) return null;
    let col = 0;
    const walker = document.createTreeWalker(code, NodeFilter.SHOW_TEXT);
    for (let n = walker.nextNode(); n && n !== node; n = walker.nextNode()) col += n.data.length;
    return { line: row.__index + 1, col: col + (node.nodeType === 3 ? off : 0) + 1, text: code.textContent };
  }
  function posAt(x, y) {
    const cp = document.caretPositionFromPoint?.(x, y);
    const r = cp ? null : document.caretRangeFromPoint?.(x, y);
    return cp ? posOf(cp.offsetNode, cp.offset) : r ? posOf(r.startContainer, r.startOffset) : null;
  }
  const IDENT = /[\p{L}\p{N}_$]/u;
  const onIdent = (p) => !!p && IDENT.test(p.text[p.col - 1] || '');
  let hoverTimer = 0;
  scroller.addEventListener('mousemove', (e) => {
    clearTimeout(hoverTimer);
    if (e.buttons || !has('nav') || inZone(e)) return;
    const { clientX: x, clientY: y } = e;
    hoverTimer = setTimeout(() => {
      const p = posAt(x, y);
      if (onIdent(p)) bus.emit('nav:hover', { path, line: p.line, col: p.col, x, y });
    }, 400);
  });
  scroller.addEventListener('mouseleave', () => clearTimeout(hoverTimer));

  // gutter: click / shift-click / drag selects whole lines
  scroller.addEventListener('mousedown', (e) => {
    if (e.button !== 0 || inZone(e)) return;
    const onGutter = e.target.closest?.('.cv-ln');
    const n = lineFromClientY(e.clientY);
    if (!onGutter) {
      const p = posAt(e.clientX, e.clientY);
      if (p) curCol = p.col;
      if ((e.metaKey || e.ctrlKey) && onIdent(p) && has('nav')) {
        e.preventDefault();
        bus.emit('nav:definition', { path, line: p.line, col: p.col });
        return;
      }
      if (!e.shiftKey) {
        cur = Math.max(1, Math.min(total, n));
        anchor = cur;
        if (selA) { selA = 0; selB = 0; }
        vl.refresh();
        emitCursor();
      }
      return;
    }
    e.preventDefault();
    scroller.focus({ preventScroll: true });
    window.getSelection()?.removeAllRanges();
    if (e.shiftKey) setCursor(n, { extend: true, reveal: false });
    else {
      anchor = n;
      setCursor(n, { extend: true, reveal: false });
    }
    const move = (ev) => setCursor(lineFromClientY(ev.clientY), { extend: true, reveal: true });
    const up = () => {
      window.removeEventListener('mousemove', move);
      window.removeEventListener('mouseup', up);
      selected();
    };
    window.addEventListener('mousemove', move);
    window.addEventListener('mouseup', up);
  });
  scroller.addEventListener('mouseup', (e) => {
    if (e.button || inZone(e) || e.target.closest?.('.cv-ln')) return;
    setTimeout(() => { if (!window.getSelection()?.isCollapsed) selected(); });
  });
  scroller.addEventListener('keyup', (e) => { if (e.key === 'Shift' && selA && !inZone(e)) selected(); });

  scroller.addEventListener('dblclick', (e) => {
    if (inZone(e)) return;
    const sel = window.getSelection()?.toString().trim();
    if (sel && /^[a-zA-Z0-9_$]+$/.test(sel) && sel.length >= 2) {
      setOccurrences(sel);
    } else {
      clearOccurrences();
    }
  });

  scroller.addEventListener('keydown', (e) => {
    if (inZone(e)) return;
    const mod = e.metaKey || e.ctrlKey;
    const page = Math.max(1, Math.floor(scroller.clientHeight / lh) - 2);
    let target = null;
    switch (e.key) {
      case 'ArrowDown': target = mod ? total : cur + 1; break;
      case 'ArrowUp': target = mod ? 1 : cur - 1; break;
      case 'PageDown': target = cur + page; break;
      case 'PageUp': target = cur - page; break;
      case 'Home': if (mod) target = 1; else { scroller.scrollLeft = 0; e.preventDefault(); return; } break;
      case 'End': if (mod) target = total; else { scroller.scrollLeft = scroller.scrollWidth; e.preventDefault(); return; } break;
      case 'Escape':
        if (selA) { selA = 0; selB = 0; vl.refresh(); emitCursor(); e.preventDefault(); e.stopPropagation(); }
        return;
      default: return;
    }
    if (e.altKey) return;
    e.preventDefault();
    setCursor(target, { extend: e.shiftKey });
  });

  let settleTimer = 0;
  scroller.addEventListener('scroll', () => {
    el.classList.toggle('scrolled', scroller.scrollTop > 2);
    zone?.place();
    clearTimeout(settleTimer);
    settleTimer = setTimeout(() => bus.emit('view:state', path), 400);
  }, { passive: true });
  scroller.addEventListener('focus', emitCursor);

  // copy whole lines when a line range is selected and there is no native text selection
  scroller.addEventListener('copy', async (e) => {
    if (inZone(e)) return;
    const sel = window.getSelection();
    if (sel && !sel.isCollapsed) return;
    if (!selA) return;
    e.preventDefault();
    const text = await linesText(selA, selB);
    navigator.clipboard?.writeText(text).catch(() => {});
    bus.emit('toast', { kind: 'ok', title: `Copied ${selB - selA + 1} lines` });
  });

  async function linesText(a, b) {
    const out = [];
    for (let from = a; from <= b; from += 1000) {
      const res = await request('file/lines', { query: { path, from, count: Math.min(1000, b - from + 1), hl: 0 } });
      for (const l of res.lines) out.push(l.text ?? '');
    }
    return out.join('\n');
  }

  // ---------- live updates ----------
  const offHl = bus.on('ev:hl', (ev) => {
    if (ev?.path !== path) return;
    chunks.clear();
    vl.refresh();
    const r = vl.visibleRange();
    ensureRange(r.first, r.last);
  });
  const offFs = bus.on('ev:fs', (ev) => {
    if (!(ev?.overflow || ev?.changes?.some((c) => c.path === path))) return;
    reload();
  });

  async function reload() {
    try {
      const m = await api.file(path);
      total = m.lines;
      chunks.clear();
      vl.setCount(total);
      if (cur > total) cur = Math.max(1, total);
      vl.refresh();
      const r = vl.visibleRange(); // onRange won't fire for the same range
      ensureRange(r.first, r.last);
      emitCursor();
    } catch { /* deleted: the editor shows a banner */ }
  }

  if (has('git.status.v2')) {
    import('./diff.js').then((m) => m.attachGutter(scroller, path, vl)).catch(() => {});
  }

  return {
    el,
    kind: 'code',
    get total() { return total; },
    get line() { return cur; },
    selection() {
      return { path, start: selA || cur, end: selB || cur, explicit: !!selA };
    },
    /** Caret position for F12 / Shift+F12: the native selection start when it is in this view. */
    position() {
      const sel = window.getSelection();
      const p = sel?.rangeCount ? posOf(sel.anchorNode, sel.anchorOffset) : null;
      const at = p || { line: cur, col: curCol, text: vl.elementFor(cur - 1)?.lastChild.textContent || '' };
      // The whole identifier around the column (for Alt+U usages).
      let a = at.col - 1;
      let b = a;
      while (a > 0 && IDENT.test(at.text[a - 1])) a--;
      while (b < at.text.length && IDENT.test(at.text[b])) b++;
      return { path, line: at.line, col: at.col, word: at.text.slice(a, b) };
    },
    focus() { scroller.focus({ preventScroll: true }); },
    gotoLine,
    selectLines(a, b) {
      anchor = a;
      setCursor(b, { extend: true });
    },
    /** Diagnostics for this file (Map line -> {severity, message}); null clears. */
    setDiagnostics(map) { diags = map; vl.refresh(); },
    /** Caret to line n with minimal scrolling; `extend` grows the line selection (vim j/k, V). */
    moveCursor(n, extend = false) { setCursor(n, { extend }); },
    path,
    /** Find matches to decorate: {byLine, cur} or null to clear. */
    setFind(d) {
      if (!d) { clearFind(); return; }
      find = d;
      paintFind();
    },
    /** Scroll a match into view without the jump flash; keeps the caret on the match line. */
    revealMatch(line, a = 0) {
      cur = line;
      anchor = line;
      selA = 0;
      selB = 0;
      const rowTop = (line - 1) * lh;
      const top = vl.virtualTop(scroller.scrollTop, scroller.clientHeight);
      if (rowTop < top + lh * 2 || rowTop > top + scroller.clientHeight - lh * 3) vl.scrollToIndex(line - 1, 'center');
      // horizontal: keep the match start visible
      const cw = measureCharWidth(el);
      const x = a * cw;
      const gutter = parseFloat(getComputedStyle(scroller).getPropertyValue('--gutter-w')) || 60;
      if (x < scroller.scrollLeft || x > scroller.scrollLeft + scroller.clientWidth - gutter - 80) scroller.scrollLeft = Math.max(0, x - (scroller.clientWidth - gutter) / 2);
      vl.refresh();
      emitCursor();
    },
    toggleWrap,
    get wrap() { return wrap; },
    setZone(z) { zone = z; zoneModel = null; fitZone(); vl.refresh(); },
    get zone() { return zone; },
    fitZone,
    layout: { scroller, sizer, vl, lh, posOf },
    /** Re-read the file (after an edit), keeping the scroll position. */
    reload,
    setOccurrences,
    clearOccurrences,
    state() { return { line: cur, scrollTop: scroller.scrollTop, wrap }; },
    restore(s) {
      if (!s) return;
      cur = s.line || 1;
      anchor = cur;
      if (s.wrap && !wrap) toggleWrap(true);
      if (s.scrollTop) {
        restoreTop = s.scrollTop;
        scroller.scrollTop = s.scrollTop;
      }
    },
    onShow() {
      // scrollTop can only be applied once the element is in the document
      if (restoreTop != null) {
        scroller.scrollTop = restoreTop;
        restoreTop = null;
      }
      vl.schedule(true);
      emitCursor();
      paintRuler();
    },
    destroy() {
      destroyed = true;
      zone?.close();
      if (find) clearFind();
      clearOccurrences();
      for (const c of pending.values()) c.abort();
      offHl();
      offFs();
      wrapper?.destroy();
      vl.destroy();
    },
  };
}
