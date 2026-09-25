// Markdown document view: rendered preview (default) with a Source toggle (Alt+M).
// The HTML comes from GET /file/markdown, sanitized by the backend (API.md § 5.4), and goes
// through the audited sink. In-repo links carry data-path/data-line; external images stay
// inert (data-ext-src) until the reader opts in; blocks carry data-line for scroll sync.
import { h, mount, setTrustedHTML } from '../core/dom.js';
import { api, isAbort } from '../core/api.js';
import { store } from '../core/store.js';
import { bus } from '../core/bus.js';
import { dirname, formatCount, joinPath } from '../core/util.js';
import { icon } from '../ui/icons.js';
import { toast } from '../ui/overlay.js';
import { createCodeView } from './viewer.js';

export function createMarkdownView(path, meta, { onOpen, preferSource = false }) {
  const article = h('article', { class: 'md' });
  const notice = h('div', { class: 'md-notice', hidden: true });
  const scroller = h('div', { class: 'md-view', tabindex: '0', role: 'document', 'aria-label': `${path} preview` }, notice, article);
  const el = h('div', { class: 'cv-wrap md-wrap' }, scroller);
  let mode = preferSource ? 'source' : 'preview';
  let code = null; // lazily created source view
  let ctrl = null;
  let loaded = false;
  let restoreTop = null;
  let pendingLine = 0;
  let headings = [];
  const listeners = new Set();

  const seg = h('div', { class: 'seg', role: 'group', 'aria-label': 'Markdown mode' },
    h('button', { 'aria-pressed': 'true', 'data-mode': 'preview', 'data-tip': 'Preview', 'data-keys': 'Alt+M', on: { click: () => setMode('preview') } }, 'Preview'),
    h('button', { 'aria-pressed': 'false', 'data-mode': 'source', 'data-tip': 'Source', 'data-keys': 'Alt+M', on: { click: () => setMode('source') } }, 'Source'));

  function syncSeg() {
    for (const b of seg.children) b.setAttribute('aria-pressed', String(b.dataset.mode === mode));
  }

  async function load() {
    ctrl?.abort();
    ctrl = new AbortController();
    try {
      const res = await api.markdown(path, { signal: ctrl.signal });
      const top = scroller.scrollTop;
      setTrustedHTML(article, res.html, 'markdown');
      headings = res.headings || [];
      decorate();
      renderNotice(res.externalImages || 0);
      loaded = true;
      if (pendingLine) { scrollToLine(pendingLine); pendingLine = 0; } else if (restoreTop != null && scroller.isConnected) { scroller.scrollTop = restoreTop; restoreTop = null; } else scroller.scrollTop = top;
    } catch (e) {
      if (isAbort(e)) return;
      mount(article, h('div', { class: 'empty' }, icon('alert', 'xl'), h('h3', null, 'Cannot render this document'), h('p', null, e.message),
        h('button', { class: 'btn sm', on: { click: () => setMode('source') } }, 'Show source')));
    }
  }

  /** Copy buttons on code blocks; nothing else is added to the sanitized markup. */
  function decorate() {
    for (const pre of article.querySelectorAll('pre')) {
      const btn = h('button', { class: 'md-copy icon-btn sm', 'aria-label': 'Copy code', 'data-tip': 'Copy' }, icon('copy', 'sm'));
      btn.addEventListener('click', async (e) => {
        e.stopPropagation();
        await navigator.clipboard?.writeText(pre.querySelector('code')?.textContent ?? pre.textContent).catch(() => {});
        mount(btn, icon('check', 'sm'));
        setTimeout(() => mount(btn, icon('copy', 'sm')), 1200);
      });
      pre.appendChild(btn);
    }
  }

  function renderNotice(n) {
    if (!n) { notice.hidden = true; return; }
    notice.hidden = false;
    mount(notice, icon('eye', 'sm'),
      h('span', null, `${formatCount(n)} external image${n === 1 ? '' : 's'} blocked to keep this document private.`),
      h('button', {
        class: 'btn sm',
        on: {
          click: () => {
            for (const img of article.querySelectorAll('img[data-ext-src]')) {
              const src = img.getAttribute('data-ext-src');
              if (/^https:\/\//i.test(src)) img.src = src;
            }
            notice.hidden = true;
          },
        },
      }, 'Load images'));
  }

  // Links: in-repo files open in ferro, #anchors scroll, http(s) opens outside.
  article.addEventListener('click', (e) => {
    const a = e.target.closest('a');
    if (!a || !article.contains(a)) return;
    const target = a.getAttribute('data-path');
    const href = a.getAttribute('href') || '';
    if (target) {
      e.preventDefault();
      const line = Number(a.getAttribute('data-line')) || undefined;
      onOpen(target, { line, preview: !(e.metaKey || e.ctrlKey), focus: true });
    } else if (href.startsWith('#')) {
      e.preventDefault();
      const id = decodeURIComponent(href.slice(1));
      article.querySelector(`[id="${CSS.escape(id)}"]`)?.scrollIntoView({ block: 'start', behavior: 'smooth' });
    } else if (/^https?:\/\//i.test(href)) {
      e.preventDefault();
      openExternal(href);
    } else if (href && !/^[a-z]+:/i.test(href)) {
      // relative link the backend did not resolve: try it against this document's folder
      e.preventDefault();
      onOpen(joinPath(dirname(path), href.split('#')[0]), { focus: true });
    }
  });

  function openExternal(url) {
    if (store.get('meta')?.host === 'desktop') {
      api.openExternal?.(url).catch(() => toast({ kind: 'error', title: 'Could not open link', message: url }));
      return;
    }
    window.open(url, '_blank', 'noopener,noreferrer');
  }

  // ---------- scroll sync via data-line ----------
  function topLine() {
    const top = scroller.getBoundingClientRect().top + 8;
    let best = 1;
    for (const b of article.querySelectorAll('[data-line]')) {
      const r = b.getBoundingClientRect();
      if (r.bottom < top) { best = Number(b.dataset.line) || best; continue; }
      if (r.top <= top) best = Number(b.dataset.line) || best;
      break;
    }
    return best;
  }

  function scrollToLine(line, { flash = false } = {}) {
    if (!loaded) { pendingLine = line; return; }
    let target = null;
    for (const b of article.querySelectorAll('[data-line]')) {
      if (Number(b.dataset.line) <= line) target = b;
      else break;
    }
    if (!target) { scroller.scrollTop = 0; return; }
    scroller.scrollTop += target.getBoundingClientRect().top - scroller.getBoundingClientRect().top - 16;
    if (flash) {
      target.classList.remove('md-flash');
      void target.offsetWidth;
      target.classList.add('md-flash');
    }
  }

  function ensureCode() {
    if (!code) {
      code = createCodeView(path, meta);
      code.el.hidden = true;
      el.appendChild(code.el);
    }
    return code;
  }

  function setMode(next, { sync = true } = {}) {
    if (next === mode && (next === 'preview' || code)) { syncSeg(); return; }
    const line = mode === 'preview' ? (loaded ? topLine() : 1) : code?.line || 1;
    mode = next;
    syncSeg();
    if (mode === 'source') {
      const c = ensureCode();
      scroller.hidden = true;
      c.el.hidden = false;
      c.onShow();
      if (sync) c.gotoLine(line, { flashIt: false });
      c.focus();
    } else {
      if (code) code.el.hidden = true;
      scroller.hidden = false;
      if (!loaded) load();
      if (sync && loaded) scrollToLine(line);
      scroller.focus({ preventScroll: true });
      emitCursor();
    }
    bus.emit('view:state', path);
    for (const fn of listeners) fn(mode);
  }

  function emitCursor() {
    store.set('cursor', { path, preview: true, language: 'Markdown', total: meta.lines, line: 1 });
  }

  // Live reload on change; headings feed the outline through /file/outline as usual.
  const offFs = bus.on('ev:fs', (ev) => {
    if ((ev?.changes || []).some((c) => c.path === path) || ev?.overflow) { if (mode === 'preview') load(); else loaded = false; }
  });

  if (mode === 'preview') load();
  else setMode('source', { sync: false });

  return {
    el,
    kind: 'markdown',
    get mode() { return mode; },
    get total() { return meta.lines; },
    get line() { return mode === 'source' ? code?.line : (loaded ? topLine() : 1); },
    get headings() { return headings; },
    toolbar() { return seg; },
    sourceView() { return mode === 'source' ? code : null; },
    toggleMode() { setMode(mode === 'preview' ? 'source' : 'preview'); },
    onModeChange(fn) { listeners.add(fn); return () => listeners.delete(fn); },
    selection() { return mode === 'source' ? code.selection() : { path, start: topLine(), end: topLine() }; },
    focus() { (mode === 'source' ? code : null)?.focus() ?? scroller.focus({ preventScroll: true }); },
    gotoLine(n, o = {}) {
      if (mode === 'source') code.gotoLine(n, o);
      else scrollToLine(n, { flash: o.flashIt !== false });
    },
    selectLines(a, b) { setMode('source'); code.selectLines(a, b); },
    state() { return { mode, scrollTop: scroller.scrollTop, line: mode === 'source' ? code?.line : undefined, source: code?.state() }; },
    restore(s) {
      if (!s) return;
      if (s.mode === 'source') { setMode('source', { sync: false }); if (s.source) code.restore(s.source); }
      else restoreTop = s.scrollTop ?? null;
    },
    onShow() {
      if (mode === 'source') { code.onShow(); return; }
      if (restoreTop != null && loaded) { scroller.scrollTop = restoreTop; restoreTop = null; }
      emitCursor();
    },
    destroy() {
      ctrl?.abort();
      offFs?.();
      code?.destroy();
      listeners.clear();
    },
  };
}
