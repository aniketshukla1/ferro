// DOM helpers. This is the ONLY module allowed to write HTML strings into the DOM
// (setTrustedHTML). Everything else builds nodes with h()/text so untrusted strings
// (file names, commit messages, comments, AI output) can never become markup.

const TRUSTED_KINDS = new Set(['hl', 'markdown']);
const ATTR_PROPS = new Set(['role', 'tabindex', 'title', 'type', 'id', 'for', 'placeholder', 'spellcheck', 'autocomplete', 'href', 'target', 'rel', 'draggable', 'name', 'value', 'min', 'max', 'step']);

/**
 * Create an element.
 * props: class, text, on:{event:fn}, dataset:{}, style:{'--x': '1px'} (CSSOM only),
 *        attrs:{}, aria-*, data-*, and common attributes; anything else is set as a property.
 * @param {string} tag
 * @param {Record<string, any> | null} [props]
 * @param {...any} children
 * @returns {HTMLElement}
 */
export function h(tag, props, ...children) {
  const el = document.createElement(tag);
  if (props) applyProps(el, props);
  append(el, children);
  return el;
}

function applyProps(el, props) {
  for (const key in props) {
    const v = props[key];
    if (v == null || v === false) continue;
    if (key === 'class') el.className = Array.isArray(v) ? v.filter(Boolean).join(' ') : v;
    else if (key === 'text') el.textContent = String(v);
    else if (key === 'on') for (const ev in v) el.addEventListener(ev, v[ev]);
    else if (key === 'dataset') Object.assign(el.dataset, v);
    else if (key === 'style') for (const p in v) el.style.setProperty(p, v[p]);
    else if (key === 'attrs') for (const a in v) setAttr(el, a, v[a]);
    else if (key.startsWith('aria-') || key.startsWith('data-') || ATTR_PROPS.has(key)) setAttr(el, key, v);
    else el[key] = v;
  }
}

function setAttr(el, name, v) {
  if (v == null || v === false) return;
  el.setAttribute(name, v === true ? '' : String(v));
}

/** Append children (nodes, strings, numbers, arrays; null/false skipped). */
export function append(el, children) {
  for (const c of children) {
    if (c == null || c === false) continue;
    if (Array.isArray(c)) append(el, c);
    else if (c instanceof Node) el.appendChild(c);
    else el.appendChild(document.createTextNode(String(c)));
  }
  return el;
}

/** Replace all children. */
export function mount(el, ...children) {
  el.replaceChildren();
  return append(el, children);
}

/**
 * The single audited HTML sink.
 * kind 'hl': syntax-highlighted lines from the server (escaped text + <span class="t-…"> only).
 * kind 'markdown': markdown sanitized by the server (docs/spec/API.md § 5.4).
 * @param {Element} el @param {string} html @param {'hl'|'markdown'} kind
 */
export function setTrustedHTML(el, html, kind) {
  if (!TRUSTED_KINDS.has(kind)) throw new Error(`refusing untrusted HTML sink: ${kind}`);
  el.innerHTML = html;
}

/** Escape text for HTML (text and attribute contexts). */
export function escapeHtml(s) {
  return String(s).replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' })[c]);
}

export const $ = (sel, root = document) => root.querySelector(sel);
export const $$ = (sel, root = document) => Array.from(root.querySelectorAll(sel));

/** Build text with <mark> around the given UTF-16 positions (fuzzy matches). */
export function markPositions(text, positions, cls) {
  const frag = document.createDocumentFragment();
  if (!positions || !positions.length) {
    frag.appendChild(document.createTextNode(text));
    return frag;
  }
  const set = new Set(positions);
  let run = '';
  let inMark = false;
  const flush = () => {
    if (!run) return;
    if (inMark) frag.appendChild(h('mark', cls ? { class: cls } : null, run));
    else frag.appendChild(document.createTextNode(run));
    run = '';
  };
  for (let i = 0; i < text.length; i++) {
    const m = set.has(i);
    if (m !== inMark) { flush(); inMark = m; }
    run += text[i];
  }
  flush();
  return frag;
}

/**
 * DOM Range covering [start, end) UTF-16 offsets of `root`'s text (for CSS Custom Highlights).
 * Text inside elements matching `skip` (e.g. a "… N chars" suffix) does not count.
 * @returns {Range|null}
 */
export function textRange(root, start, end, skip = '.cv-cut') {
  const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT, {
    acceptNode: (n) => (skip && n.parentElement?.closest(skip) ? NodeFilter.FILTER_REJECT : NodeFilter.FILTER_ACCEPT),
  });
  let pos = 0;
  let range = null;
  for (let n = walker.nextNode(); n; n = walker.nextNode()) {
    const len = n.data.length;
    if (!range && start <= pos + len) {
      range = document.createRange();
      range.setStart(n, Math.max(0, start - pos));
    }
    if (range && end <= pos + len) {
      range.setEnd(n, Math.max(0, end - pos));
      return range;
    }
    pos += len;
  }
  return null;
}

/** Build text with <mark> around [start,end) UTF-16 ranges (search hits). */
export function markRanges(text, ranges) {
  const frag = document.createDocumentFragment();
  let pos = 0;
  for (const [a, b] of ranges || []) {
    if (a > pos) frag.appendChild(document.createTextNode(text.slice(pos, a)));
    frag.appendChild(h('mark', null, text.slice(a, b)));
    pos = b;
  }
  if (pos < text.length) frag.appendChild(document.createTextNode(text.slice(pos)));
  return frag;
}

/** Is focus inside a text-entry control? */
export function inTextField(el = document.activeElement) {
  if (!el) return false;
  const tag = el.tagName;
  return tag === 'INPUT' || tag === 'TEXTAREA' || tag === 'SELECT' || el.isContentEditable;
}
