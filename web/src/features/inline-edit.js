// Inline edit (API.md § 4.10, § 10.9; FRONTEND.md § 6.25): a box under the selected lines. Type
// the new code, or tell the AI what to change and review its diff, then save; the toast undoes it.
// The viewer only keeps the box's slot (setZone) and reports selections (`view:select`).
import { h, mount, setTrustedHTML } from '../core/dom.js';
import { request, has } from '../core/api.js';
import { store } from '../core/store.js';
import { basename, debounce, isMac, plural } from '../core/util.js';
import { keysEl } from '../core/keys.js';
import { icon } from '../ui/icons.js';
import { toast } from '../ui/overlay.js';

const MAX_LINES = 2000;
const MOD = isMac ? 'metaKey' : 'ctrlKey';
const lines = (a, b) => (b <= a ? `line ${a}` : `lines ${a}–${b}`);
let bar = null;

/** The lines to edit: a text selection in the code, else the line selection, else the caret line. */
function editRange(view) {
  const { scroller, posOf } = view.layout;
  const s = window.getSelection();
  if (s && !s.isCollapsed && scroller.contains(s.anchorNode) && scroller.contains(s.focusNode)) {
    const [p, q] = [posOf(s.anchorNode, s.anchorOffset), posOf(s.focusNode, s.focusOffset)];
    if (p && q) {
      const [lo, hi] = p.line <= q.line ? [p, q] : [q, p];
      // Stopping at the start of a line (a triple-click) leaves that line out.
      return { start: lo.line, end: hi.line > lo.line && hi.col === 1 ? hi.line - 1 : hi.line, explicit: true };
    }
  }
  return view.selection();
}

function hideBar() { bar?.remove(); bar = null; }

/** After a selection: Edit / Edit with AI under it, until the selection or the view changes. */
export function offerBar(editor) {
  hideBar();
  const view = editor.activeView();
  if (view?.kind !== 'code' || view.zone || !view.layout) return;
  const r = editRange(view);
  if (!r.explicit) return;
  const { scroller, sizer, vl } = view.layout;
  const go = (ai) => () => { hideBar(); inlineEdit(editor, ai); };
  const el = h('div', { class: 'cv-selbar', role: 'toolbar', 'aria-label': `Edit ${lines(r.start, r.end)}` },
    h('button', { class: 'btn sm', type: 'button', 'data-tip': 'Edit these lines (Alt+I)', on: { click: go(false) } }, icon('pencil', 'sm'), 'Edit'),
    has('ai.edit') ? h('button', { class: 'btn sm', type: 'button', 'data-tip': 'Tell the AI what to change (Alt+K)', on: { click: go(true) } }, icon('sparkles', 'sm'), 'Edit with AI') : null);
  sizer.appendChild(el);
  const place = () => {
    const x = Math.max(scroller.scrollLeft + 8, scroller.scrollLeft + scroller.clientWidth - el.offsetWidth - 28);
    el.style.transform = `translate(${x}px, ${vl.rowTop(r.end - 1) + vl.rowH(r.end - 1) + 4}px)`;
  };
  place();
  const off = () => { scroller.removeEventListener('scroll', place); scroller.removeEventListener('mousedown', hide); scroller.removeEventListener('keydown', hide); unsub(); };
  const hide = (e) => { if (!e?.target?.closest?.('.cv-selbar')) { off(); if (bar === el) hideBar(); } };
  const unsub = store.subscribe('cursor', (c) => { if (c?.path !== view.path || !editRange(view).explicit) hide(); });
  scroller.addEventListener('scroll', place, { passive: true });
  scroller.addEventListener('mousedown', hide);
  scroller.addEventListener('keydown', hide);
  bar = el;
}

/** Open the box for the active code view's selection (or caret line); `ai` focuses the AI prompt. */
export async function inlineEdit(editor, ai = false) {
  hideBar();
  const view = editor.activeView();
  if (view?.kind !== 'code' || !view.layout) return toast({ title: 'Open a file and select the lines to edit', timeout: 2500 });
  if (store.get('meta')?.readOnly) return toast({ kind: 'warn', title: 'ferro is read-only here', message: 'It was started with --read-only, so files cannot be changed.' });
  if (view.zone) return view.zone.focus(ai);
  const { start, end } = editRange(view);
  if (end - start >= MAX_LINES) return toast({ kind: 'warn', title: `Select at most ${MAX_LINES.toLocaleString()} lines to edit here`, timeout: 3000 });
  let res;
  try {
    res = await request('file/lines', { query: { path: view.path, from: start, count: end - start + 1, hl: 0 } });
  } catch (e) {
    return toast({ kind: 'error', title: 'Cannot read these lines', message: e.message });
  }
  if (res.lines.some((l) => l.cut)) return toast({ kind: 'warn', title: 'A line here is too long to edit in ferro', message: 'Lines over 4,000 characters are cut in the viewer; edit this one in your editor.' });
  // Past the end of the file (or an empty file): insert instead of replace.
  openBox(view, start, start + res.lines.length - 1, res.lines.map((l) => l.text ?? '').join('\n'), ai);
}

function openBox(view, start, end, original, ai) {
  const { path } = view;
  const { scroller, sizer, vl, lh } = view.layout;
  if (vl.scaled) return toast({ kind: 'warn', title: 'This file is too long to edit inline', timeout: 3000 });
  let base = original; // what the file holds for the range (updated after a 409)
  let aiBefore = null;
  let busy = false;
  let abort = null;
  let armed = false;
  const unit = /^\t/m.test(original) ? '\t' : /^ {2}(?! )/m.test(original) && !/^ {4}/m.test(original) ? '  ' : '    ';

  const input = h('textarea', { class: 'ie-input', spellcheck: 'false', autocapitalize: 'off', autocomplete: 'off', wrap: 'off', 'aria-label': `New code for ${lines(start, end)} of ${basename(path)}` });
  input.value = original;
  const hl = h('pre', { class: 'ie-hl', 'aria-hidden': 'true' });
  const code = h('div', { class: 'ie-code' }, hl, input);
  const prompt = h('input', { class: 'input sm ie-prompt', type: 'text', placeholder: 'Tell the AI what to change, e.g. "handle the empty list"', 'aria-label': 'Instruction for the AI' });
  const genBtn = h('button', { class: 'btn sm', type: 'button', on: { click: () => (busy ? abort?.abort() : generate()) } });
  const note = h('span', { class: 'ie-ai-note faint small', role: 'status' });
  const undoAi = h('button', { class: 'link-btn small', type: 'button', hidden: true, on: { click: () => { input.value = aiBefore; undoAi.hidden = true; note.textContent = 'AI change undone.'; sync(); input.focus(); } } }, 'Undo AI change');
  const aiRow = h('div', { class: 'ie-ai', hidden: !ai }, prompt, genBtn, h('div', { class: 'ie-ai-status' }, note, undoAi));
  const aiBtn = has('ai.edit') && h('button', { class: 'btn ghost sm', type: 'button', 'data-tip': `Ask the AI (${isMac ? '⌥K' : 'Alt+K'})`, on: { click: () => focus(true) } }, icon('sparkles', 'sm'), 'Ask AI');
  const diffEl = h('div', { class: 'ie-diff', hidden: true, role: 'region', 'aria-label': 'Changes' });
  const diffBtn = h('button', { class: 'btn ghost sm', type: 'button', 'aria-pressed': 'false', on: { click: () => showDiff(diffEl.hidden) } }, 'Changes');
  const saveBtn = h('button', { class: 'btn primary sm', type: 'button', on: { click: () => save() } }, 'Save');
  const err = h('p', { class: 'ie-err', role: 'alert', hidden: true });
  const dot = () => h('span', { class: 'ie-dot' }, '·');
  const box = h('div', { class: 'ie', role: 'group', 'aria-label': `Edit ${basename(path)}` },
    h('div', { class: 'ie-head' }, icon('pencil', 'sm'), h('span', { class: 'ie-title' }, `Editing ${lines(start, end)}`), h('span', { class: 'ie-sp' }),
      aiBtn || null, diffBtn, h('button', { class: 'btn ghost sm', type: 'button', on: { click: () => close() } }, 'Cancel'), saveBtn),
    aiRow, code, diffEl, err,
    h('div', { class: 'ie-foot faint small' }, keysEl('Mod+S'), ' save', dot(), keysEl('Escape'), ' cancel',
      aiBtn ? [dot(), keysEl('Alt+K'), ' ask AI'] : null, dot(), 'Saving writes the file; you can undo it.'));

  // The slot: the range's last row grows by the box's height and the box lies over that space.
  const slot = h('div', { class: 'cv-zone' }, box);
  const zone = {
    start, end: Math.max(start, end), h: 0, focus, close,
    place() {
      const i = zone.end - 1;
      slot.style.transform = `translate(${scroller.scrollLeft}px, ${vl.rowTop(i) + vl.rowH(i) - zone.h}px)`;
      slot.style.width = `${scroller.clientWidth}px`;
    },
  };
  const ro = new ResizeObserver(() => { const hh = Math.ceil(slot.offsetHeight); if (hh !== zone.h) { zone.h = hh; view.fitZone(); } });
  sizer.appendChild(slot);
  ro.observe(slot);
  view.setZone(zone);

  // Colors: plain text at once, the server's highlighting 120 ms after typing stops.
  let seq = 0;
  const paint = debounce(async () => {
    const my = ++seq;
    const value = input.value;
    try {
      const r = await request('highlight', { method: 'POST', body: { code: value, path } });
      if (my === seq && value === input.value) setTrustedHTML(hl, `${r.lines.join('\n')}\n`, 'hl');
    } catch { /* plain text stays */ }
  }, 120);
  function sync() {
    hl.textContent = `${input.value}\n`; // the newline keeps an empty last line's height
    // The textarea takes the colored layer's size: the box scrolls sideways, never the textarea.
    input.style.width = `${Math.max(hl.offsetWidth, code.clientWidth)}px`;
    input.style.height = `${hl.offsetHeight}px`;
    paint();
    saveBtn.disabled = busy || input.value === base;
    box.classList.toggle('dirty', input.value !== base);
    if (!diffEl.hidden) renderDiff();
    armed = false;
  }
  function renderDiff() {
    const rows = lineDiff(base ? base.split('\n') : [], input.value ? input.value.split('\n') : []);
    const n = (t) => rows.filter((r) => r.t === t).length;
    mount(diffEl, h('div', { class: 'ie-diff-head faint small' }, n('add') || n('del') ? `+${n('add')} −${n('del')}` : 'No changes yet'),
      rows.map((r) => h('div', { class: `ie-drow ${r.t}` }, h('span', { class: 'ie-dmark', 'aria-hidden': 'true' }, { add: '+', del: '−' }[r.t] || ' '), h('span', null, r.text || ' '))));
  }
  function showDiff(on) {
    diffEl.hidden = !on;
    diffBtn.setAttribute('aria-pressed', String(on));
    if (on) renderDiff();
  }
  /** Keep the range and the box on screen (the box's bottom wins when both do not fit). */
  function reveal() {
    const top = vl.rowTop(start - 1);
    const bottom = vl.rowTop(zone.end - 1) + vl.rowH(zone.end - 1);
    if (bottom > scroller.scrollTop + scroller.clientHeight - lh) scroller.scrollTop = Math.max(0, Math.min(top - lh, bottom - scroller.clientHeight + lh));
    else if (top < scroller.scrollTop) scroller.scrollTop = Math.max(0, top - lh);
  }
  function focus(wantAi) {
    reveal();
    if (wantAi && aiBtn) { aiRow.hidden = false; prompt.focus(); prompt.select(); } else input.focus();
  }
  function setBusy(on) {
    busy = on;
    input.readOnly = on;
    box.classList.toggle('busy', on);
    mount(genBtn, on ? [icon('stop', 'sm'), 'Stop'] : [icon('sparkles', 'sm'), 'Generate']);
    saveBtn.disabled = on || input.value === base;
  }
  function fail(msg, extra) { mount(err, msg, extra || null); err.hidden = false; }

  async function generate() {
    const instruction = prompt.value.trim();
    if (!instruction) { note.textContent = 'Write what to change first.'; return prompt.focus(); }
    const before = input.value;
    let acc = '';
    let final = null;
    let error = null;
    abort = new AbortController();
    setBusy(true);
    note.textContent = 'Writing…';
    err.hidden = true;
    try {
      const { aiApi } = await import('../core/ai-api.js');
      await aiApi.edit({ path, startLine: start, endLine: end, instruction, text: before }, {
        signal: abort.signal,
        onEvent: (ev, d) => {
          // An opening code fence stays hidden while it streams; the final text has none.
          if (ev === 'token') { acc += d?.text || ''; input.value = acc.replace(/^```[^\n]*\n?/, ''); sync(); }
          else if (ev === 'final') final = d?.text ?? '';
          else if (ev === 'error') error = d?.message || 'The AI could not edit these lines.';
        },
      });
    } catch (e) {
      error = e.message;
    }
    setBusy(false);
    if (final == null || error) {
      input.value = before;
      note.textContent = abort.signal.aborted && !error ? 'Stopped.' : '';
      sync();
      if (error || !abort.signal.aborted) fail(error || 'The AI did not answer.');
      return;
    }
    input.value = final;
    aiBefore = before;
    undoAi.hidden = false;
    note.textContent = 'Review the changes below, edit them if you like, then Save.';
    sync();
    showDiff(true);
    input.focus({ preventScroll: true });
    setTimeout(reveal, 60); // after the box has grown by the diff
  }

  async function save() {
    const text = input.value;
    if (busy) return;
    if (text === base) return close();
    setBusy(true);
    err.hidden = true;
    const write = (b) => request('file/edit', { method: 'POST', body: { path, ...b } });
    try {
      const r = await write({ startLine: start, endLine: end, expected: base, text });
      const was = base;
      close(true);
      view.reload();
      toast({
        kind: 'ok', title: `Saved ${basename(path)}`, timeout: 8000,
        message: `${lines(start, end)} → ${r.endLine < r.startLine ? 'removed' : plural(r.endLine - r.startLine + 1, 'line')}`,
        // Undo writes the old lines back while the new ones are still there.
        action: {
          label: 'Undo',
          run: () => write({ startLine: r.startLine, endLine: r.endLine, expected: text, text: was }).then(
            () => { view.reload(); toast({ kind: 'ok', title: 'Edit undone', timeout: 2000 }); },
            (e) => toast({ kind: 'error', title: 'Could not undo', message: e.status === 409 ? 'The lines changed again since the save.' : e.message })),
        },
      });
    } catch (e) {
      setBusy(false);
      const cur = e.detail?.current;
      if (e.status !== 409) return fail(e.message);
      fail('These lines changed on disk since you opened them. ', typeof cur === 'string'
        ? h('button', { class: 'link-btn small', type: 'button', on: { click: () => { base = cur; err.hidden = true; showDiff(true); sync(); } } }, 'Compare with the file now') : null);
    }
  }

  function close(saved = false) {
    abort?.abort();
    ro.disconnect();
    slot.remove();
    if (view.zone === zone) view.setZone(null);
    view.gotoLine(start, { flashIt: saved }); // the box is gone: bring the lines back into view
    view.focus();
  }
  function escape() {
    if (busy) return abort?.abort();
    if (input.value !== base && !armed) { armed = true; return fail('Press Esc again to discard your changes.'); }
    close();
  }

  input.addEventListener('input', sync);
  input.addEventListener('keydown', (e) => keys(e, input, unit, save, escape));
  prompt.addEventListener('keydown', (e) => {
    if (e.key === 'Enter' && !e.isComposing) { e.preventDefault(); if (!busy) generate(); } else if (e.key === 'Escape') { e.preventDefault(); busy ? abort?.abort() : input.focus(); } else if (e[MOD] && e.key.toLowerCase() === 's') { e.preventDefault(); save(); }
  });
  // Alt+K inside the box; every other key stays inside it (not the viewer's, not vim's).
  box.addEventListener('keydown', (e) => { if (e.altKey && e.code === 'KeyK') { e.preventDefault(); focus(true); } e.stopPropagation(); });
  setBusy(false);
  sync();
  requestAnimationFrame(() => focus(ai));
}

// Tab / Shift+Tab indent and outdent the lines a selection touches, Enter keeps the indent.
function keys(e, ta, unit, save, escape) {
  if (e.isComposing) return;
  if (e[MOD] && (e.key.toLowerCase() === 's' || e.key === 'Enter')) { e.preventDefault(); return save(); }
  if (e.key === 'Escape') { e.preventDefault(); return escape(); }
  if (ta.readOnly || !(e.key === 'Tab' || (e.key === 'Enter' && !e.shiftKey && !e.altKey && !e[MOD]))) return;
  e.preventDefault();
  const { selectionStart: a, selectionEnd: b, value: v } = ta;
  const from = v.lastIndexOf('\n', a - 1) + 1;
  if (e.key === 'Enter') return insert(ta, a, b, `\n${v.slice(from, a).match(/^[ \t]*/)[0]}`);
  if (!e.shiftKey && a === b) return insert(ta, a, b, unit);
  const out = new RegExp(`^(\\t| {1,${unit.length === 1 ? 4 : unit.length}})`);
  const next = v.slice(from, b).split('\n').map((l) => (e.shiftKey ? l.replace(out, '') : unit + l)).join('\n');
  insert(ta, from, b, next);
  ta.setSelectionRange(from, from + next.length);
}

/** Replace a range through the editing stack, so the browser's own undo keeps working. */
function insert(ta, a, b, text) {
  ta.setSelectionRange(a, b);
  if (!document.execCommand?.('insertText', false, text)) {
    ta.setRangeText(text, a, b, 'end');
    ta.dispatchEvent(new Event('input'));
  }
}

/** Line diff (LCS) as [{t: 'ctx'|'del'|'add', text}]; a middle over 1M cells is all del + all add. */
export function lineDiff(a, b) {
  let p = 0;
  let s = 0;
  while (p < a.length && p < b.length && a[p] === b[p]) p++;
  while (s < a.length - p && s < b.length - p && a[a.length - 1 - s] === b[b.length - 1 - s]) s++;
  const row = (t) => (text) => ({ t, text });
  const A = a.slice(p, a.length - s);
  const B = b.slice(p, b.length - s);
  const out = a.slice(0, p).map(row('ctx'));
  const n = A.length;
  const m = B.length;
  if (n * m > 1e6) out.push(...A.map(row('del')), ...B.map(row('add')));
  else {
    const L = new Uint32Array((n + 1) * (m + 1));
    const at = (i, j) => L[i * (m + 1) + j];
    for (let i = n - 1; i >= 0; i--) for (let j = m - 1; j >= 0; j--) L[i * (m + 1) + j] = A[i] === B[j] ? at(i + 1, j + 1) + 1 : Math.max(at(i + 1, j), at(i, j + 1));
    for (let i = 0, j = 0; i < n || j < m;) {
      if (i < n && j < m && A[i] === B[j]) { out.push({ t: 'ctx', text: A[i] }); i++; j++; } else if (j < m && (i >= n || at(i, j + 1) > at(i + 1, j))) out.push({ t: 'add', text: B[j++] });
      else out.push({ t: 'del', text: A[i++] });
    }
  }
  return out.concat(a.slice(a.length - s).map(row('ctx')));
}
