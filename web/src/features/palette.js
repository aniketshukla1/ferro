// Command palette: one input, prefix modes (FRONTEND.md § 6.6).
//   (none) files + blended content hits · > commands · @ symbols · # workspace symbols
//   : go to line · % text search · ? help · theme picker (live preview) · pick (a caller's list)
import { h, mount, markPositions, markRanges } from '../core/dom.js';
import { api, has, isAbort, request } from '../core/api.js';
import { store } from '../core/store.js';
import { listCommands, execute } from '../core/commands.js';
import { keysEl } from '../core/keys.js';
import { rank } from '../core/match.js';
import { basename, dirname, formatMs, plural } from '../core/util.js';
import { reviewUrlIn } from '../core/text.js';
import { icon, fileIcon } from '../ui/icons.js';
import { THEMES, currentTheme, previewTheme, setTheme, resolveTheme, loadThemePack } from './themes.js';
import { THEME_NOTES } from './theme-notes.js';
import { session } from './session.js';
import { KIND_ICON } from './nav.js';

const MODES = [
  { prefix: '>', id: 'commands', label: 'Commands', icon: 'command', hint: 'Run a command', placeholder: 'Type a command…' },
  { prefix: '@', id: 'symbols', label: 'Symbols', icon: 'at', hint: 'Go to symbol in file', placeholder: 'Symbol in this file…' },
  { prefix: '#', id: 'wsymbols', label: 'Workspace symbols', icon: 'hash', hint: 'Symbols across the workspace', placeholder: 'Symbol in workspace…' },
  { prefix: ':', id: 'line', label: 'Go to line', icon: 'enter', hint: 'Jump to a line', placeholder: 'Line number, optionally :column' },
  { prefix: '%', id: 'search', label: 'Search', icon: 'search', hint: 'Search text in files', placeholder: 'Search text in all files…' },
  { prefix: '?', id: 'help', label: 'Help', icon: 'help', hint: 'What can the palette do?', placeholder: '' },
];

export function createPalette({ editor, onOpen }) {
  let overlay = null;
  let box = null;
  let input = null;
  let list = null;
  let modeTag = null;
  let footTiming = null;
  let items = [];
  let activeIdx = -1;
  let seq = 0;
  let renderedSeq = 0; // seq that `items` currently reflects; lags `seq` while a query is in flight
  let pendingEnter = null; // an Enter keydown that arrived before renderedSeq caught up
  let ctrl = null;
  let special = null; // 'theme' | 'pick'
  let pick = null; // { title, items } for special 'pick'
  let themeBefore = null;
  let prevFocus = null;
  const outlineCache = new Map();

  function modeOf(value) {
    if (special) return { id: special, query: value };
    const m = MODES.find((x) => value.startsWith(x.prefix));
    return m ? { id: m.id, query: value.slice(m.prefix.length).trimStart(), mode: m } : { id: 'files', query: value };
  }

  // ---------- open / close ----------
  function open(prefix = '', opts = {}) {
    special = opts.special || null;
    pick = special === 'pick' ? { title: opts.title || 'Choose', items: opts.items || [] } : null;
    if (overlay) {
      input.value = prefix;
      update();
      input.focus();
      return;
    }
    prevFocus = document.activeElement;
    input = h('input', {
      class: 'pal-input',
      type: 'text',
      spellcheck: 'false',
      autocomplete: 'off',
      role: 'combobox',
      'aria-expanded': 'true',
      'aria-controls': 'pal-list',
      'aria-autocomplete': 'list',
      'aria-label': 'Command palette',
    });
    modeTag = h('span', { class: 'pal-mode', hidden: true });
    list = h('div', { class: 'pal-list', id: 'pal-list', role: 'listbox' });
    footTiming = h('span', { class: 'timing' });
    const modeChips = MODES.filter((m) => m.id !== 'help').map((m) => h('button', {
      class: 'mode-chip',
      'data-tip': m.hint,
      'data-tip-side': 'top',
      on: { click: () => { input.value = m.prefix; update(); input.focus(); } },
    }, m.prefix));
    box = h('div', { class: 'palette', role: 'dialog', 'aria-label': 'Command palette' },
      h('div', { class: 'pal-input-row' }, icon('search'), modeTag, input),
      list,
      h('div', { class: 'pal-foot' },
        h('span', { class: 'hint' }, keysEl('ArrowUp', 'plain'), keysEl('ArrowDown', 'plain'), 'navigate'),
        h('span', { class: 'hint' }, keysEl('Enter', 'plain'), 'open'),
        h('span', { class: 'hint' }, keysEl('Escape', 'plain'), 'close'),
        footTiming,
        h('span', { class: 'modes' }, modeChips)));
    overlay = h('div', { class: 'overlay', on: { mousedown: (e) => { if (e.target === overlay) close(); } } }, box);
    document.body.appendChild(overlay);
    input.addEventListener('input', update);
    input.addEventListener('keydown', onKey);
    input.value = prefix;
    input.placeholder = placeholderFor();
    input.focus();
    update();
  }

  function close({ restore = true } = {}) {
    if (!overlay) return;
    pendingEnter = null;
    ctrl?.abort();
    if (special === 'theme' && themeBefore != null) previewTheme(themeBefore);
    overlay.remove();
    overlay = null;
    box = null;
    special = null;
    pick = null;
    themeBefore = null;
    if (restore && prevFocus?.isConnected) prevFocus.focus?.({ preventScroll: true });
  }

  function placeholderFor() {
    if (special === 'theme') return 'Choose a theme — arrows preview live';
    if (special === 'pick') return 'Filter…';
    const m = modeOf(input.value);
    return m.mode?.placeholder || 'Search files by name — or type > @ : % ?';
  }

  // ---------- keyboard ----------
  function onKey(e) {
    if (e.key === 'ArrowDown') { e.preventDefault(); move(1); }
    else if (e.key === 'ArrowUp') { e.preventDefault(); move(-1); }
    else if (e.key === 'PageDown') { e.preventDefault(); move(8); }
    else if (e.key === 'PageUp') { e.preventDefault(); move(-8); }
    else if (e.key === 'Enter') {
      e.preventDefault();
      // A query is still in flight for the latest keystroke: run once its results render,
      // instead of silently dropping Enter against the previous (stale) item list.
      if (renderedSeq !== seq) pendingEnter = e;
      else run(items[activeIdx], e);
    }
    else if (e.key === 'Escape') { e.preventDefault(); e.stopPropagation(); close(); }
    else if (e.key === 'Backspace' && special && !input.value) { special = null; update(); }
  }

  function move(d) {
    const selectable = items.map((it, i) => (it.group ? -1 : i)).filter((i) => i >= 0);
    if (!selectable.length) return;
    let pos = selectable.indexOf(activeIdx);
    pos = pos < 0 ? 0 : Math.max(0, Math.min(selectable.length - 1, pos + d));
    if (d === 1 && selectable.indexOf(activeIdx) === selectable.length - 1) pos = 0;
    if (d === -1 && selectable.indexOf(activeIdx) === 0) pos = selectable.length - 1;
    setActive(selectable[pos]);
  }

  function setActive(i) {
    activeIdx = i;
    for (const el of list.children) {
      const on = Number(el.dataset.i) === i;
      el.classList.toggle('active', on);
      el.setAttribute('aria-selected', String(on));
      if (on) {
        el.scrollIntoView({ block: 'nearest' });
        input.setAttribute('aria-activedescendant', el.id);
      }
    }
    items[i]?.preview?.();
  }

  function run(it, e) {
    if (!it || it.group) return;
    if (it.keepOpen) { it.run(e); return; }
    close({ restore: false });
    it.run(e);
  }

  // ---------- render ----------
  function render(newItems, { keepActive = false } = {}) {
    // Whenever render() actually runs, it is for the current `seq`: sync modes call it inline
    // within update(), and async modes guard on `my !== seq` before reaching here.
    renderedSeq = seq;
    const prevKey = keepActive ? items[activeIdx]?.key : null;
    items = newItems;
    const els = items.map((it, i) => {
      if (it.group) return h('div', { class: 'pal-group', role: 'presentation', 'data-i': String(i) }, it.group);
      return h('div', {
        class: 'pal-item',
        role: 'option',
        id: `pal-opt-${i}`,
        'data-i': String(i),
        'aria-selected': 'false',
        on: {
          mousemove: () => { if (activeIdx !== i) setActive(i); },
          mousedown: (e) => e.preventDefault(),
          click: (e) => run(it, e),
        },
      },
      h('span', { class: 'pi-icon' }, it.icon || null),
      h('span', { class: 'pi-main' },
        h('span', { class: 'pi-label' }, it.labelPos ? markPositions(it.label, it.labelPos) : it.label),
        it.code != null ? h('span', { class: 'pi-code' }, markRanges(it.code, it.codeRanges)) : null,
        it.desc ? h('span', { class: 'pi-desc' }, it.descPos ? markPositions(it.desc, it.descPos) : it.desc) : null),
      it.right ? h('span', { class: 'pi-right' }, it.right) : null);
    });
    if (!items.some((x) => !x.group)) els.push(h('div', { class: 'pal-empty', role: 'option', 'aria-disabled': 'true' }, emptyText()));
    mount(list, els);
    let idx = prevKey ? items.findIndex((x) => x.key === prevKey) : -1;
    if (idx < 0) idx = items.findIndex((x) => !x.group);
    if (idx >= 0) setActive(idx);
    else activeIdx = -1;
    list.offsetHeight;
    box.__onPaint?.();
    if (pendingEnter && renderedSeq === seq) {
      const e = pendingEnter;
      pendingEnter = null;
      run(items[activeIdx], e);
    }
  }

  let emptyReason = '';
  function emptyText() {
    return emptyReason || 'No matches';
  }

  // ---------- update per mode ----------
  function update() {
    const value = input.value;
    const m = modeOf(value);
    const my = ++seq;
    // A new query supersedes any Enter that was waiting on the previous one's results.
    pendingEnter = null;
    ctrl?.abort();
    ctrl = new AbortController();
    emptyReason = '';
    input.placeholder = placeholderFor();
    if (m.mode || special) {
      const def = m.mode || (pick ? { label: pick.title, icon: 'at' } : { label: 'Theme', icon: 'contrast' });
      mount(modeTag, icon(def.icon, 'xs'), def.label);
      modeTag.hidden = false;
    } else modeTag.hidden = true;

    switch (m.id) {
      case 'files': return filesMode(m.query, my);
      case 'commands': return commandsMode(m.query);
      case 'symbols': return symbolsMode(m.query, my);
      case 'wsymbols': return wsymbolsMode(m.query, my);
      case 'pick': return pickMode(m.query);
      case 'line': return lineMode(m.query);
      case 'search': return searchMode(m.query, my);
      case 'help': return helpMode();
      case 'theme': return themeMode(m.query);
      default: return render([]);
    }
  }

  function fileItem(path, positions, extra = {}) {
    const base = basename(path);
    const dir = dirname(path);
    const cut = path.length - base.length;
    return {
      key: `f:${path}`,
      icon: fileIcon(path),
      label: base,
      labelPos: positions?.filter((p) => p >= cut).map((p) => p - cut),
      desc: dir,
      descPos: positions?.filter((p) => p < cut - 1),
      run: (e) => onOpen(path, { preview: false, focus: true, line: extra.line, side: e?.metaKey || e?.ctrlKey || e?.altKey }),
      ...extra,
    };
  }

  async function filesMode(q, my) {
    const lineMatch = /^(.*?):(\d+)(?::(\d+))?$/.exec(q);
    const query = lineMatch ? lineMatch[1] : q;
    const line = lineMatch ? Number(lineMatch[2]) : undefined;
    if (!query) {
      const recent = session.data.recent.filter((p) => p !== editor.active).slice(0, 12);
      const out = recent.length ? [{ group: 'Recently opened' }, ...recent.map((p) => fileItem(p, null, { right: icon('history', 'sm') }))] : [];
      emptyReason = 'Type to search files · > for commands · ? for help';
      render(out);
      return;
    }
    const prUrl = reviewUrlIn(q);
    if (prUrl) {
      const mr = prUrl.includes('/-/merge_requests/');
      render([{ group: mr ? 'Merge request' : 'Pull request' }, {
        key: 'pr',
        icon: icon('git-pull-request'),
        label: `Open ${prUrl}`,
        desc: has(mr ? 'pr.gitlab' : 'pr.open') ? 'review in ferro' : `in-app ${mr ? 'MR' : 'PR'} review is coming soon`,
        run: () => execute('pr.open', prUrl),
      }]);
      return;
    }
    try {
      const res = await api.fuzzy(query, 40, session.data.recent.slice(0, 20), { signal: ctrl.signal });
      if (my !== seq) return;
      const out = [];
      if (res.results.length) out.push({ group: `Files · ${plural(res.total, 'match', 'matches')}` });
      for (const r of res.results) out.push(fileItem(r.path, r.positions, { line }));
      footTiming.textContent = res.ms != null ? `${formatMs(res.ms)}` : '';
      render(out);
      if (query.length >= 3 && !line && (has('search.v2') || !has('v1'))) blendContent(query, my, out);
    } catch (e) {
      if (!isAbort(e) && my === seq) { emptyReason = e.message; render([]); }
    }
  }

  async function blendContent(q, my, fileItems) {
    try {
      const params = { q, mode: 'literal', case: 'smart', maxFiles: 6, maxPerFile: 1 };
      const res = await api.search(params, { signal: ctrl.signal });
      if (my !== seq || !res.files.length) return;
      const content = [{ group: `In files · ${formatMs(res.ms)}` }];
      for (const f of res.files) {
        const hit = f.hits[0];
        content.push({
          key: `c:${f.path}:${hit.line}`,
          icon: fileIcon(f.path),
          label: `${basename(f.path)}:${hit.line}`,
          code: hit.text.trim() ? hit.text.replace(/^\s+/, '') : hit.text,
          codeRanges: shiftRanges(hit.text, hit.ranges),
          run: () => onOpen(f.path, { preview: false, focus: true, line: hit.line }),
        });
      }
      render([...fileItems, ...content], { keepActive: true });
    } catch { /* content hits are best-effort */ }
  }

  function shiftRanges(text, ranges) {
    const lead = text.length - text.replace(/^\s+/, '').length;
    return (ranges || []).map(([a, b]) => [Math.max(0, a - lead), Math.max(0, b - lead)]);
  }

  function commandsMode(q) {
    const cmds = listCommands();
    const ranked = rank(q, cmds, (c) => (c.category ? `${c.category}: ${c.title}` : c.title), 60);
    render(ranked.map(({ item: c, positions }) => ({
      key: `cmd:${c.id}`,
      icon: icon(c.icon || 'command', 'sm'),
      label: c.category ? `${c.category}: ${c.title}` : c.title,
      labelPos: positions,
      right: c.keys?.[0] ? keysEl(c.keys[0]) : null,
      run: () => execute(c.id),
    })));
  }

  async function symbolsMode(q, my) {
    const path = editor.active;
    if (!path) { emptyReason = 'Open a file to see its symbols'; render([]); return; }
    let outline = outlineCache.get(path);
    if (!outline) {
      try {
        outline = await api.outline(path, { signal: ctrl.signal });
        outlineCache.set(path, outline);
        setTimeout(() => outlineCache.delete(path), 30_000);
      } catch (e) {
        if (!isAbort(e) && my === seq) { emptyReason = 'No outline for this file'; render([]); }
        return;
      }
    }
    if (my !== seq) return;
    if (!outline.symbols.length) emptyReason = 'No symbols found in this file';
    const ranked = rank(q, outline.symbols, (s) => s.name, 200);
    render(ranked.map(({ item: s, positions }) => ({
      key: `sym:${s.line}:${s.name}`,
      icon: icon(KIND_ICON[s.kind] || 'dot', 'sm'),
      label: s.name,
      labelPos: positions,
      desc: s.kind,
      right: h('span', { class: 'num' }, `:${s.line}`),
      run: () => editor.activeView()?.gotoLine?.(s.line),
      preview: () => editor.activeView()?.gotoLine?.(s.line, { flashIt: false }),
    })));
  }

  // `#`: workspace symbols from the B6 index (API.md § 4.8).
  async function wsymbolsMode(q, my) {
    if (!has('symbols')) { emptyReason = 'Workspace symbols need a ferro server with the symbol index. Use @ for symbols in this file.'; render([]); return; }
    if (!q) { emptyReason = 'Type a symbol name'; render([]); return; }
    let res;
    try {
      res = await request('symbols', { query: { q, limit: 60 }, signal: ctrl.signal, label: 'symbols' });
    } catch (e) {
      if (!isAbort(e) && my === seq) { emptyReason = e.message; render([]); }
      return;
    }
    if (my !== seq) return;
    const syms = res?.symbols || [];
    footTiming.textContent = res?.ms != null ? formatMs(res.ms) : '';
    render(syms.map((sym) => ({
      key: `ws:${sym.path}:${sym.line}:${sym.name}`,
      icon: icon(KIND_ICON[sym.kind] || 'dot', 'sm'),
      label: sym.name,
      labelPos: rank(q, [sym], (x) => x.name, 1)[0]?.positions,
      desc: `${sym.path}:${sym.line}`,
      right: h('span', { class: 'faint small' }, sym.kind),
      run: (e) => onOpen(sym.path, { preview: false, focus: true, line: sym.line, side: e?.metaKey || e?.ctrlKey || e?.altKey }),
    })));
  }

  // A caller-supplied list (e.g. several definitions of one name), filtered by the query.
  function pickMode(q) {
    const list = pick?.items || [];
    const ranked = q ? rank(q, list, (it) => it.text || it.label, 200).map(({ item }) => item) : list;
    render(ranked);
  }

  function lineMode(q) {
    const view = editor.activeView();
    if (!view?.gotoLine) { emptyReason = 'Open a file first'; render([]); return; }
    const m = /^(\d+)(?::(\d+))?$/.exec(q.trim());
    const total = view.total;
    if (!m) {
      emptyReason = `Current line ${view.line} of ${total}. Type a line number between 1 and ${total}.`;
      render([]);
      return;
    }
    const n = Math.min(total, Math.max(1, Number(m[1])));
    render([{ key: 'line', icon: icon('enter', 'sm'), label: `Go to line ${n}`, desc: `of ${total}`, run: () => view.gotoLine(n, { select: true }) }]);
  }

  async function searchMode(q, my) {
    if (q.length < 2) { emptyReason = 'Type at least 2 characters to search file contents'; render([]); return; }
    try {
      const params = { q, mode: 'literal', case: 'smart', maxFiles: 40, maxPerFile: 3 };
      const res = await api.search(params, { signal: ctrl.signal });
      if (my !== seq) return;
      const out = [];
      for (const f of res.files) {
        out.push({ group: f.path });
        for (const hit of f.hits) {
          out.push({
            key: `s:${f.path}:${hit.line}`,
            icon: h('span', { class: 'num faint' }, String(hit.line)),
            label: '',
            code: hit.text.replace(/^\s+/, ''),
            codeRanges: shiftRanges(hit.text, hit.ranges),
            run: () => onOpen(f.path, { preview: false, focus: true, line: hit.line }),
          });
        }
      }
      footTiming.textContent = `${plural(res.filesMatched ?? res.files.length, 'file')} · ${formatMs(res.ms)}`;
      emptyReason = `No results for “${q}”`;
      render(out);
    } catch (e) {
      if (!isAbort(e) && my === seq) { emptyReason = e.message; render([]); }
    }
  }

  function helpMode() {
    render([{ group: 'Palette modes' },
      { key: 'h:files', icon: icon('search', 'sm'), label: 'Files', desc: 'type a name — no prefix', keepOpen: true, run: () => { input.value = ''; update(); } },
      ...MODES.filter((m) => m.id !== 'help').map((m) => ({
        key: `h:${m.id}`,
        icon: icon(m.icon, 'sm'),
        label: `${m.prefix}  ${m.label}`,
        desc: m.hint,
        keepOpen: true,
        run: () => { input.value = m.prefix; update(); input.focus(); },
      })),
      { group: 'Tips' },
      { key: 'h:line', icon: icon('enter', 'sm'), label: 'path:line', desc: 'open a file at a line, e.g. server.rs:120', keepOpen: true, run: () => { input.value = ''; update(); } },
      { key: 'h:keys', icon: icon('keyboard', 'sm'), label: 'All keyboard shortcuts', right: keysEl('?'), run: () => execute('help.keys') },
    ]);
  }

  function themeMode(q) {
    if (themeBefore == null) { themeBefore = currentTheme(); loadThemePack(); }
    const active = resolveTheme(currentTheme());
    const all = [{ id: 'auto', name: 'Auto (match system)', type: 'system' }, ...THEMES];
    const ranked = rank(q, all, (t) => t.name, 50);
    render(ranked.map(({ item: t, positions }) => ({
      key: `t:${t.id}`,
      icon: swatch(t.id),
      label: t.name,
      labelPos: positions,
      desc: THEME_NOTES[t.id] || t.type,
      right: (t.id === themeBefore || (themeBefore !== 'auto' && t.id === active && t.id === themeBefore)) ? icon('check', 'sm') : null,
      preview: () => previewTheme(t.id),
      run: () => { themeBefore = null; setTheme(t.id); },
    })), { keepActive: false });
    const idx = items.findIndex((it) => it.key === `t:${themeBefore}`);
    if (idx >= 0) setActive(idx);
  }

  function swatch(id) {
    const s = h('span', { class: 'swatch' });
    s.dataset.theme = resolveTheme(id);
    s.append(h('i', null), h('i', null), h('i', null));
    return s;
  }

  return {
    open,
    close,
    get isOpen() { return !!overlay; },
  };
}
