// Code viewer: virtualized rows fetched in 500-line chunks from /file/lines (hl=1),
// sticky gutter, caret line, line-range selection (gutter drag / Shift+click / Shift+arrows),
// jump-to-line with a flash, exact-highlight refresh on `hl` events.
import { h, mount, setTrustedHTML, textRange } from '../core/dom.js';
import { api, isAbort, request } from '../core/api.js';
import { VirtualList } from '../core/virtual.js';
import { store } from '../core/store.js';
import { bus } from '../core/bus.js';
import { LRU, formatBytes, formatCount } from '../core/util.js';
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
  let restoreTop = null;
  let destroyed = false;
  const lh = lineHeight();

  const vl = new VirtualList({
    scroller, sizer, rowHeight: lh, overscan: 24,
    create: createRow, update: updateRow, onRange: ensureRange, onPaint: paintFind,
  });

  // ---------- find decorations (CSS Custom Highlight API; one active view owns the registry) ----------
  /** @type {{byLine: Map<number, number[][]>, cur: {line:number, k:number}|null}|null} */
  let find = null;
  const canHighlight = typeof Highlight === 'function' && !!globalThis.CSS?.highlights;
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
  }

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
    const line = lineAt(n);
    if (line) {
      if (row.__html !== line.html || row.__cut !== line.cut) {
        setTrustedHTML(code, line.html ?? '', 'hl');
        if (line.cut) code.appendChild(h('span', { class: 'cv-cut' }, `… ${formatCount(line.cut)} chars`));
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
    return Math.floor((vTop + (y - rect.top)) / lh) + 1;
  }

  // gutter: click / shift-click / drag selects whole lines
  scroller.addEventListener('mousedown', (e) => {
    if (e.button !== 0) return;
    const onGutter = e.target.closest?.('.cv-ln');
    const n = lineFromClientY(e.clientY);
    if (!onGutter) {
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
    };
    window.addEventListener('mousemove', move);
    window.addEventListener('mouseup', up);
  });

  scroller.addEventListener('keydown', (e) => {
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
    clearTimeout(settleTimer);
    settleTimer = setTimeout(() => bus.emit('view:state', path), 400);
  }, { passive: true });
  scroller.addEventListener('focus', emitCursor);

  // copy whole lines when a line range is selected and there is no native text selection
  scroller.addEventListener('copy', async (e) => {
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
    } catch { /* deleted: the editor shows a banner */ }
  }

  return {
    el,
    kind: 'code',
    get total() { return total; },
    get line() { return cur; },
    selection() {
      return { path, start: selA || cur, end: selB || cur };
    },
    focus() { scroller.focus({ preventScroll: true }); },
    gotoLine(n, { select = false, flashIt = true } = {}) {
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
    },
    selectLines(a, b) {
      anchor = a;
      setCursor(b, { extend: true });
    },
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
    state() { return { line: cur, scrollTop: scroller.scrollTop }; },
    restore(s) {
      if (!s) return;
      cur = s.line || 1;
      anchor = cur;
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
    },
    destroy() {
      destroyed = true;
      if (find) clearFind();
      for (const c of pending.values()) c.abort();
      offHl();
      offFs();
      vl.destroy();
    },
  };
}

const ZOOMS = [0.05, 0.1, 0.25, 0.5, 0.75, 1, 1.5, 2, 3, 4, 6, 8, 12, 16, 24, 32];

/**
 * Image view: fit / 1:1 / step zoom (buttons, +/-, Ctrl or ⌘ + wheel, pinch) around the pointer,
 * drag to pan, pixelated rendering past 200 %, checkerboard or plain background.
 */
export function createImageView(path, meta) {
  const img = h('img', { alt: path, src: api.rawUrl(path), draggable: 'false' });
  const stage = h('div', { class: 'imgv-stage', tabindex: '0', role: 'img', 'aria-label': path }, img);
  const zoomText = h('span', { class: 'num imgv-zoom' }, '—');
  const dims = h('span', { class: 'num' }, 'Loading…');
  const btn = (ic, label, keys, fn) => h('button', { class: 'icon-btn sm', 'aria-label': label, 'data-tip': label, 'data-keys': keys, on: { click: fn } }, icon(ic, 'sm'));
  const fitBtn = h('button', { class: 'btn ghost sm', 'aria-pressed': 'true', 'data-tip': 'Fit to window', 'data-keys': '0', on: { click: () => fit() } }, 'Fit');
  const oneBtn = h('button', { class: 'btn ghost sm', 'aria-pressed': 'false', 'data-tip': 'Actual size', 'data-keys': '1', on: { click: () => setZoom(1) } }, '1:1');
  const bgBtn = btn('contrast', 'Toggle background', null, () => stage.classList.toggle('plain'));
  const bar = h('div', { class: 'imgv-bar' },
    dims, h('span', { class: 'imgv-sp' }),
    btn('minus', 'Zoom out', '-', () => step(-1)), zoomText, btn('plus', 'Zoom in', '+', () => step(1)),
    h('span', { class: 'fb-sep' }), fitBtn, oneBtn, bgBtn);
  const el = h('div', { class: 'imgv' }, stage, bar);
  let zoom = 1;
  let fitMode = true;

  function fitScale() {
    if (!img.naturalWidth) return 1;
    const pad = 48;
    return Math.min(1, (stage.clientWidth - pad) / img.naturalWidth, (stage.clientHeight - pad) / img.naturalHeight) || 1;
  }
  function apply(anchor) {
    const before = { w: img.width || 1, sl: stage.scrollLeft, st: stage.scrollTop };
    img.style.width = `${Math.max(1, Math.round(img.naturalWidth * zoom))}px`;
    img.style.height = `${Math.max(1, Math.round(img.naturalHeight * zoom))}px`;
    img.classList.toggle('pixelated', zoom >= 2);
    zoomText.textContent = `${Math.round(zoom * 100)}%`;
    fitBtn.setAttribute('aria-pressed', String(fitMode));
    oneBtn.setAttribute('aria-pressed', String(!fitMode && zoom === 1));
    if (anchor) {
      // keep the point under the cursor fixed while zooming
      const k = (img.naturalWidth * zoom) / before.w;
      stage.scrollLeft = (before.sl + anchor.x) * k - anchor.x;
      stage.scrollTop = (before.st + anchor.y) * k - anchor.y;
    }
  }
  function fit() { fitMode = true; zoom = fitScale(); apply(); }
  function setZoom(z, anchor) { fitMode = false; zoom = Math.min(32, Math.max(0.05, z)); apply(anchor); }
  function step(dir, anchor) {
    const next = dir > 0 ? ZOOMS.find((z) => z > zoom + 1e-6) : [...ZOOMS].reverse().find((z) => z < zoom - 1e-6);
    if (next) setZoom(next, anchor);
  }

  img.addEventListener('load', () => {
    dims.textContent = `${img.naturalWidth} × ${img.naturalHeight} · ${formatBytes(meta.size)}`;
    fit();
  });
  img.addEventListener('error', () => { dims.textContent = 'Cannot display this image'; });
  stage.addEventListener('wheel', (e) => {
    if (!e.ctrlKey && !e.metaKey) return; // plain wheel scrolls; pinch arrives as ctrl+wheel
    e.preventDefault();
    const r = stage.getBoundingClientRect();
    setZoom(zoom * Math.exp(-e.deltaY * 0.01), { x: e.clientX - r.left, y: e.clientY - r.top });
  }, { passive: false });
  stage.addEventListener('pointerdown', (e) => {
    if (e.button !== 0 || (stage.scrollWidth <= stage.clientWidth && stage.scrollHeight <= stage.clientHeight)) return;
    const x0 = e.clientX; const y0 = e.clientY; const sl = stage.scrollLeft; const st = stage.scrollTop;
    stage.setPointerCapture(e.pointerId);
    stage.classList.add('panning');
    const move = (ev) => { stage.scrollLeft = sl - (ev.clientX - x0); stage.scrollTop = st - (ev.clientY - y0); };
    const up = () => { stage.classList.remove('panning'); stage.removeEventListener('pointermove', move); stage.removeEventListener('pointerup', up); };
    stage.addEventListener('pointermove', move);
    stage.addEventListener('pointerup', up);
  });
  stage.addEventListener('keydown', (e) => {
    if (e.metaKey || e.ctrlKey || e.altKey) return;
    if (e.key === '+' || e.key === '=') { e.preventDefault(); step(1); }
    else if (e.key === '-') { e.preventDefault(); step(-1); }
    else if (e.key === '0') { e.preventDefault(); fit(); }
    else if (e.key === '1') { e.preventDefault(); setZoom(1); }
  });
  const ro = new ResizeObserver(() => { if (fitMode && img.naturalWidth) fit(); });
  ro.observe(stage);

  return {
    el,
    kind: 'image',
    focus() { stage.focus({ preventScroll: true }); },
    state: () => (fitMode ? null : { zoom }),
    restore(s) { if (s?.zoom) { fitMode = false; zoom = s.zoom; } },
    onShow() { store.set('cursor', { path, image: true, language: meta.language || 'Image' }); if (fitMode && img.naturalWidth) fit(); },
    destroy() { ro.disconnect(); },
  };
}
