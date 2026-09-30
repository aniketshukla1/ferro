// Vim keys for the code viewer (FRONTEND.md § 6.18; ui.keymap = "vim", off by default).
// The viewer is read-only, so this is motion and selection only: counts, j/k, gg/G, Ctrl-d/u/f/b,
// zz, / n N, marks (m{a-z} and '{a-z}), gd, V with j/k and y, Esc.
import { h, inTextField } from '../core/dom.js';
import { execute } from '../core/commands.js';

let state = null;

/** Turn vim keys on (idempotent). `statusEl` gets the mode badge. */
export function enableVim({ editor, statusEl }) {
  if (state) return;
  const badge = h('span', { class: 'vim-mode', 'aria-live': 'polite' }, 'NORMAL');
  statusEl?.prepend(badge);
  state = { editor, badge, mode: 'normal', count: '', pending: '', marks: new Map() };
  document.addEventListener('keydown', onKey, true);
}

export function disableVim() {
  if (!state) return;
  document.removeEventListener('keydown', onKey, true);
  state.badge.remove();
  state = null;
}

function setMode(mode) {
  state.mode = mode;
  state.badge.textContent = mode === 'visual' ? 'VISUAL LINE' : 'NORMAL';
  state.badge.classList.toggle('visual', mode === 'visual');
}

function onKey(e) {
  const view = state.editor.activeView();
  // Only while the code view itself has focus; inputs, the palette and dialogs keep their keys.
  if (view?.kind !== 'code' || !e.target.closest?.('.cv') || inTextField(e.target) || e.metaKey || e.altKey) return;
  const ctrl = e.ctrlKey;
  const k = e.key;
  const s = state;
  const n = Number(s.count) || 1;
  const take = () => { e.preventDefault(); e.stopPropagation(); s.count = ''; };
  const move = (line) => view.moveCursor(Math.max(1, Math.min(view.total, line)), s.mode === 'visual');
  const half = Math.max(1, Math.floor((e.target.clientHeight || 600) / 40));

  if (s.pending === 'm' || s.pending === "'" || s.pending === '`') {
    const p = s.pending;
    s.pending = '';
    if (/^[a-z]$/.test(k)) {
      take();
      const marks = s.marks.get(view.path) || new Map();
      if (p === 'm') { marks.set(k, view.line); s.marks.set(view.path, marks); }
      else if (marks.has(k)) move(marks.get(k));
    }
    return;
  }
  if (s.pending === 'g') {
    s.pending = '';
    if (k === 'g') { const c = Number(s.count); take(); move(c || 1); return; }
    if (k === 'd') { take(); execute('nav.definition'); return; }
  }
  if (s.pending === 'z') {
    s.pending = '';
    if (k === 'z') { take(); view.gotoLine(view.line, { flashIt: false }); return; }
  }
  if (ctrl) {
    const d = { d: half, u: -half, f: half * 2, b: -half * 2 }[k];
    if (d) { take(); move(view.line + d * n); }
    return;
  }
  if (/^[0-9]$/.test(k) && (k !== '0' || s.count)) { e.preventDefault(); e.stopPropagation(); s.count += k; return; }
  switch (k) {
    case 'j': case 'ArrowDown': take(); move(view.line + n); return;
    case 'k': case 'ArrowUp': take(); move(view.line - n); return;
    case 'G': { const c = Number(s.count); take(); move(c || view.total); return; }
    case 'g': case 'm': case "'": case '`': case 'z': e.preventDefault(); e.stopPropagation(); s.pending = k; return;
    case '/': take(); execute('find.open'); return;
    case 'n': take(); execute('find.next'); return;
    case 'N': take(); execute('find.prev'); return;
    case 'V': take(); if (s.mode === 'visual') { setMode('normal'); view.moveCursor(view.line); } else { setMode('visual'); view.selectLines(view.line, view.line); } return;
    case 'y':
      if (s.mode === 'visual') { take(); document.execCommand('copy'); setMode('normal'); view.moveCursor(view.line); }
      return;
    case 'Escape':
      if (s.mode === 'visual' || s.count || s.pending) { take(); s.pending = ''; if (s.mode === 'visual') { setMode('normal'); view.moveCursor(view.line); } }
      return;
    default:
  }
}
