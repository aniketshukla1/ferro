// Keyboard shortcuts. Specs look like "Mod+Shift+K", "Alt+W", "F12", "?".
// Letters/digits/punctuation match on e.code so Option-combos work on macOS
// (Option+C produces "ç" in e.key). Browser-reserved keys are only bound in the desktop host.
import { isMac } from './util.js';
import { inTextField } from './dom.js';

const PUNCT = { ',': 'Comma', '.': 'Period', '/': 'Slash', '\\': 'Backslash', ';': 'Semicolon', "'": 'Quote', '`': 'Backquote', '-': 'Minus', '=': 'Equal', '[': 'BracketLeft', ']': 'BracketRight' };
const NAMED = new Set(['Escape', 'Enter', 'Tab', 'Space', 'Backspace', 'Delete', 'Home', 'End', 'PageUp', 'PageDown', 'ArrowUp', 'ArrowDown', 'ArrowLeft', 'ArrowRight', 'F1', 'F2', 'F3', 'F4', 'F5', 'F6', 'F7', 'F8', 'F9', 'F10', 'F11', 'F12']);
const ALIASES = { Esc: 'Escape', Up: 'ArrowUp', Down: 'ArrowDown', Left: 'ArrowLeft', Right: 'ArrowRight', Return: 'Enter' };

/** @returns {{mod:boolean, ctrl:boolean, alt:boolean, shift:boolean, code?:string, key?:string, raw:string}} */
export function parseKeys(spec) {
  const parts = spec.split('+');
  // allow "Mod++" style specs for the plus key
  if (spec.endsWith('++')) { parts.pop(); parts[parts.length - 1] = '+'; }
  const out = { mod: false, ctrl: false, alt: false, shift: false, raw: spec };
  for (const p0 of parts) {
    const p = ALIASES[p0] || p0;
    if (p === 'Mod') out.mod = true;
    else if (p === 'Ctrl') out.ctrl = true;
    else if (p === 'Alt' || p === 'Option') out.alt = true;
    else if (p === 'Shift') out.shift = true;
    else if (p === 'Cmd' || p === 'Meta') out.mod = true;
    else if (/^[A-Za-z]$/.test(p)) out.code = `Key${p.toUpperCase()}`;
    else if (/^[0-9]$/.test(p)) out.code = `Digit${p}`;
    else if (PUNCT[p]) out.code = PUNCT[p];
    else if (NAMED.has(p)) out.key = p;
    else out.key = p; // "?" etc.
  }
  return out;
}

export function matches(e, k) {
  const mod = isMac ? e.metaKey : e.ctrlKey;
  // On macOS Ctrl is its own modifier; elsewhere "Ctrl" and "Mod" are the same key.
  const wantMod = k.mod || (!isMac && k.ctrl);
  if (wantMod !== mod) return false;
  if (isMac && k.ctrl !== e.ctrlKey) return false;
  if (k.alt !== e.altKey) return false;
  if (k.key && k.key.length === 1 && !/[a-z0-9]/i.test(k.key)) {
    // symbol keys like "?" imply shift on most layouts: ignore shift, compare the character
    return e.key === k.key;
  }
  if (k.shift !== e.shiftKey) return false;
  if (k.code) return e.code === k.code;
  if (k.key === 'Space') return e.code === 'Space';
  return e.key === k.key;
}

const MAC_LABELS = { mod: '⌘', ctrl: '⌃', alt: '⌥', shift: '⇧' };
const PC_LABELS = { mod: 'Ctrl', ctrl: 'Ctrl', alt: 'Alt', shift: 'Shift' };
const KEY_LABELS = { Escape: 'Esc', Enter: '↵', ArrowUp: '↑', ArrowDown: '↓', ArrowLeft: '←', ArrowRight: '→', Backspace: '⌫', Delete: 'Del', PageUp: 'PgUp', PageDown: 'PgDn', Space: 'Space', Tab: 'Tab' };

/** Key cap labels for display: "Mod+Shift+F" → ["⌘","⇧","F"] on macOS. */
export function keyLabels(spec) {
  const k = parseKeys(spec);
  const L = isMac ? MAC_LABELS : PC_LABELS;
  const caps = [];
  if (k.ctrl) caps.push(L.ctrl);
  if (k.alt) caps.push(L.alt);
  if (k.shift) caps.push(L.shift);
  if (k.mod) caps.push(L.mod);
  if (isMac) caps.sort((a, b) => '⌃⌥⇧⌘'.indexOf(a) - '⌃⌥⇧⌘'.indexOf(b));
  let main = '';
  if (k.code) {
    if (k.code.startsWith('Key')) main = k.code.slice(3);
    else if (k.code.startsWith('Digit')) main = k.code.slice(5);
    else main = Object.keys(PUNCT).find((c) => PUNCT[c] === k.code) || k.code;
  } else if (k.key) main = KEY_LABELS[k.key] || k.key;
  if (main) caps.push(main);
  return caps;
}

/** Plain text label ("⌘⇧F" / "Ctrl+Shift+F"). */
export const keyText = (spec) => (isMac ? keyLabels(spec).join('') : keyLabels(spec).join('+'));

// ---------- global keymap ----------
const bindings = [];

/**
 * Bind a shortcut.
 * @param {string} spec
 * @param {(e: KeyboardEvent) => any} handler  return false to let the event continue
 * @param {{when?:()=>boolean, inInput?:boolean, host?:'desktop'|'browser', id?:string}} [opts]
 */
export function bindKey(spec, handler, opts = {}) {
  const entry = { spec, k: parseKeys(spec), handler, ...opts };
  bindings.push(entry);
  return () => {
    const i = bindings.indexOf(entry);
    if (i >= 0) bindings.splice(i, 1);
  };
}

let hostGetter = () => 'browser';
export function setHostGetter(fn) { hostGetter = fn; }

function onKeyDown(e) {
  if (e.defaultPrevented || e.isComposing) return;
  const typing = inTextField(e.target);
  // Later bindings win (features registered later are more specific).
  for (let i = bindings.length - 1; i >= 0; i--) {
    const b = bindings[i];
    if (b.host && b.host !== hostGetter()) continue;
    if (typing && !b.inInput) continue;
    if (!matches(e, b.k)) continue;
    if (b.when && !b.when()) continue;
    const r = b.handler(e);
    if (r === false) continue;
    e.preventDefault();
    e.stopPropagation();
    return;
  }
}

let installed = false;
export function installKeymap() {
  if (installed) return;
  installed = true;
  window.addEventListener('keydown', onKeyDown);
}

/** Build a <span class="keys"><kbd>…</kbd></span> element. */
export function keysEl(spec, extraClass = '') {
  const span = document.createElement('span');
  span.className = `keys ${extraClass}`.trim();
  for (const cap of keyLabels(spec)) {
    const k = document.createElement('kbd');
    k.textContent = cap;
    span.appendChild(k);
  }
  return span;
}
