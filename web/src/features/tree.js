// File tree: lazy (GET /tree per directory), virtualized, compact folder chains,
// git badges, keyboard navigation and type-ahead.
import { h, mount } from '../core/dom.js';
import { api } from '../core/api.js';
import { store } from '../core/store.js';
import { bus } from '../core/bus.js';
import { VirtualList } from '../core/virtual.js';
import { execute } from '../core/commands.js';
import { dirname } from '../core/util.js';
import { icon, fileIcon, folderIcon } from '../ui/icons.js';
import { session } from './session.js';

/** Show a folder (or file) in the Files panel. */
export function revealDir(path) {
  execute('panel.files');
  bus.emit('tree:reveal', { path });
}

// Row height is a design token (--h-row); read it once so CSS stays the single source.
const rowHeight = () => parseFloat(getComputedStyle(document.documentElement).getPropertyValue('--h-row')) || 26;

export function createTree(container, { onOpen }) {
  const ROW_H = rowHeight();
  /** @type {Map<string, any>} */
  const nodes = new Map();
  nodes.set('', { path: '', name: '', dir: true, open: true, children: null, loading: false });
  let rows = [];
  let focusIdx = 0;
  let kbd = false;
  let current = null;
  let gitMap = new Map();
  let dirtyDirs = new Set();
  const openDirs = new Set(session.data.openDirs || []);

  const scroller = h('div', { class: 'tree', role: 'tree', tabindex: '0', 'aria-label': 'Files' });
  const skeleton = h('div', { class: 'tree-skel' }, [70, 55, 82, 48, 64, 58, 76].map((w) => {
    const s = h('span', { class: 'skel' });
    s.style.width = `${w}%`;
    return s;
  }));
  mount(container, skeleton, scroller);

  const vl = new VirtualList({ scroller, rowHeight: ROW_H, overscan: 16, create: createRow, update: updateRow });

  // Rows keep their child nodes and are patched in place: replacing nodes between
  // mousedown and mouseup would swallow the click.
  function createRow() {
    const el = h('div', { class: 'tree-row', role: 'treeitem' },
      h('span', { class: 'indent' }),
      h('span', { class: 'twisty' }, icon('chevron-right', 'xs')),
      h('span', { class: 'icon-slot' }),
      h('span', { class: 'tname' }),
      h('span', { class: 'tmeta' }));
    el.addEventListener('click', (e) => onRowClick(el.__index, e));
    el.addEventListener('dblclick', () => onRowDblClick(el.__index));
    return el;
  }

  function updateRow(el, i) {
    const r = rows[i];
    if (!r) return;
    const n = r.node;
    const git = n.dir ? null : gitMap.get(n.path);
    const [indent, twisty, slot, nameEl, meta] = el.children;
    let cls = 'tree-row';
    if (n.dir) cls += ' dir';
    if (n.ignored) cls += ' ignored';
    if (git) cls += ` git-${git}`;
    if (n.path === current) cls += ' current';
    if (i === focusIdx) cls += ' focused';
    if (el.className !== cls) el.className = cls;
    el.setAttribute('aria-level', String(r.depth + 1));
    el.setAttribute('aria-selected', String(n.path === current));
    if (n.dir) el.setAttribute('aria-expanded', String(!!n.open));
    else el.removeAttribute('aria-expanded');
    el.title = n.path;
    indent.style.setProperty('--depth', String(r.depth));
    twisty.classList.toggle('none', !n.dir);

    const iconKey = n.dir ? (n.open ? 'dir-open' : 'dir') : n.path;
    if (el.__icon !== iconKey) {
      mount(slot, n.dir ? folderIcon(!!n.open) : fileIcon(n.path));
      el.__icon = iconKey;
    }
    if (el.__label !== r.label) {
      el.__label = r.label;
      if (r.label.includes('/')) {
        mount(nameEl, r.label.split('/').flatMap((p, k) => (k ? [h('span', { class: 'compact-sep' }, '/'), p] : [p])));
      } else nameEl.textContent = r.label;
    }
    const metaKey = n.loading ? 'loading' : git ? `g:${git}` : (n.dir && !n.open && dirtyDirs.has(n.path)) ? 'dirty' : '';
    if (el.__meta !== metaKey) {
      el.__meta = metaKey;
      if (metaKey === 'loading') mount(meta, h('span', { class: 'spinner loading' }));
      else if (git) mount(meta, h('span', { class: `gitc ${git === '?' ? 'A' : git}`, title: gitTitle(git) }, git === '?' ? 'U' : git));
      else if (metaKey === 'dirty') mount(meta, h('span', { class: 'ddot', title: 'Contains changes' }));
      else mount(meta);
    }
  }

  function gitTitle(c) {
    return { M: 'Modified', A: 'Added', D: 'Deleted', R: 'Renamed', C: 'Copied', T: 'Type changed', U: 'Conflict', '?': 'Untracked' }[c] || c;
  }

  // ---------- model ----------
  async function load(dirPath) {
    const node = nodes.get(dirPath);
    if (!node || node.loading) return node?.pending;
    node.loading = true;
    render();
    node.pending = (async () => {
      try {
        const res = await api.tree(dirPath);
        node.children = res.entries.map((e) => {
          const prev = nodes.get(e.path);
          const n = { ...e, open: prev?.open ?? (e.dir && openDirs.has(e.path)), children: prev?.children ?? null, loading: false };
          nodes.set(e.path, n);
          return e.path;
        });
        // Compact chains: a directory whose only child is a directory loads through.
        if (node.children.length === 1 && nodes.get(node.children[0]).dir && !nodes.get(node.children[0]).ignored) {
          await load(node.children[0]);
        }
        const opens = node.children.filter((p) => nodes.get(p).dir && nodes.get(p).open && !nodes.get(p).children);
        await Promise.all(opens.map(load));
      } catch (e) {
        node.error = e.message;
      } finally {
        node.loading = false;
        render();
      }
    })();
    return node.pending;
  }

  function chainTail(n) {
    let tail = n;
    let label = n.name;
    while (tail.dir && !tail.ignored && tail.children && tail.children.length === 1) {
      const only = nodes.get(tail.children[0]);
      if (!only?.dir || only.ignored) break;
      tail = only;
      label += `/${only.name}`;
    }
    return { tail, label };
  }

  function flatten() {
    const out = [];
    const walk = (dirPath, depth) => {
      const node = nodes.get(dirPath);
      for (const p of node?.children || []) {
        const { tail, label } = chainTail(nodes.get(p));
        out.push({ node: tail, label, depth });
        if (tail.dir && tail.open && tail.children) walk(tail.path, depth + 1);
      }
    };
    walk('', 0);
    return out;
  }

  let renderQueued = false;
  function render() {
    if (renderQueued) return;
    renderQueued = true;
    queueMicrotask(() => {
      renderQueued = false;
      rows = flatten();
      skeleton.hidden = nodes.get('').children !== null;
      focusIdx = Math.min(focusIdx, Math.max(0, rows.length - 1));
      vl.setCount(rows.length);
      vl.refresh();
    });
  }

  function setOpen(node, open) {
    node.open = open;
    if (open) openDirs.add(node.path); else openDirs.delete(node.path);
    session.update({ openDirs: [...openDirs] });
    if (open && !node.children) load(node.path);
    render();
  }

  // ---------- interaction ----------
  function onRowClick(i, e) {
    const r = rows[i];
    if (!r) return;
    focusIdx = i;
    setKbd(false);
    const n = r.node;
    if (n.dir) setOpen(n, !n.open);
    else onOpen(n.path, { preview: !(e.metaKey || e.ctrlKey), focus: false });
    vl.refresh();
  }

  function onRowDblClick(i) {
    const n = rows[i]?.node;
    if (n && !n.dir) onOpen(n.path, { preview: false, focus: true });
  }

  function setKbd(on) {
    kbd = on;
    scroller.classList.toggle('kbd', on);
  }

  function moveFocus(i) {
    focusIdx = Math.max(0, Math.min(rows.length - 1, i));
    setKbd(true);
    vl.scrollToIndex(focusIdx);
    vl.refresh();
  }

  let typeBuf = '';
  let typeTimer = 0;
  scroller.addEventListener('keydown', (e) => {
    if (e.metaKey || e.ctrlKey || e.altKey) return;
    const r = rows[focusIdx];
    const n = r?.node;
    const page = Math.max(1, Math.floor(scroller.clientHeight / ROW_H) - 1);
    switch (e.key) {
      case 'ArrowDown': moveFocus(focusIdx + 1); break;
      case 'ArrowUp': moveFocus(focusIdx - 1); break;
      case 'PageDown': moveFocus(focusIdx + page); break;
      case 'PageUp': moveFocus(focusIdx - page); break;
      case 'Home': moveFocus(0); break;
      case 'End': moveFocus(rows.length - 1); break;
      case 'ArrowRight':
        if (n?.dir && !n.open) setOpen(n, true);
        else if (n?.dir) moveFocus(focusIdx + 1);
        break;
      case 'ArrowLeft':
        if (n?.dir && n.open) setOpen(n, false);
        else if (r) {
          for (let k = focusIdx - 1; k >= 0; k--) if (rows[k].depth < r.depth) { moveFocus(k); break; }
        }
        break;
      case 'Enter':
        if (n?.dir) setOpen(n, !n.open);
        else if (n) onOpen(n.path, { preview: false, focus: true });
        break;
      case ' ':
        if (n && !n.dir) onOpen(n.path, { preview: true, focus: false });
        else if (n?.dir) setOpen(n, !n.open);
        break;
      default:
        if (e.key.length === 1 && /\S/.test(e.key)) {
          typeBuf += e.key.toLowerCase();
          clearTimeout(typeTimer);
          typeTimer = setTimeout(() => { typeBuf = ''; }, 700);
          const start = typeBuf.length === 1 ? focusIdx + 1 : focusIdx;
          for (let k = 0; k < rows.length; k++) {
            const idx = (start + k) % rows.length;
            if (rows[idx].label.toLowerCase().startsWith(typeBuf)) { moveFocus(idx); break; }
          }
        } else return;
    }
    e.preventDefault();
  });
  scroller.addEventListener('keyup', () => setKbd(true));
  scroller.addEventListener('blur', () => setKbd(false));

  // ---------- reveal / current ----------
  function ensure(dirPath) {
    const node = nodes.get(dirPath);
    if (!node) return Promise.resolve();
    if (node.loading) return node.pending;
    if (node.children) return Promise.resolve();
    return load(dirPath);
  }

  async function reveal(path, { focus = false, align = 'center' } = {}) {
    await ensure('');
    const parts = path.split('/');
    let dir = '';
    for (let i = 0; i < parts.length - 1; i++) {
      dir = dir ? `${dir}/${parts[i]}` : parts[i];
      const node = nodes.get(dir);
      if (!node) break;
      if (!node.open) { node.open = true; openDirs.add(dir); }
      await ensure(dir);
    }
    session.update({ openDirs: [...openDirs] });
    rows = flatten();
    vl.setCount(rows.length);
    const idx = rows.findIndex((r) => r.node.path === path);
    if (idx >= 0) {
      focusIdx = idx;
      vl.scrollToIndex(idx, align);
      vl.refresh();
      if (focus) scroller.focus();
    }
  }

  // `now`: the panel can be created after these slices were set (lazy panels).
  store.subscribe('active', (p) => {
    current = p;
    vl.refresh();
  }, { now: true });
  store.subscribe('git', (g) => {
    gitMap = new Map();
    dirtyDirs = new Set();
    for (const f of g?.files || []) {
      const code = f.conflicted ? 'U' : f.untracked ? '?' : (f.worktree || f.index || 'M');
      gitMap.set(f.path, code);
      let d = f.dir ? f.path : dirname(f.path);
      while (d) { dirtyDirs.add(d); d = dirname(d); }
    }
    vl.refresh();
  }, { now: true });
  bus.on('tree:reveal', ({ path, focus, align }) => reveal(path, { focus, align }));
  bus.on('ev:workspace', () => reset());
  bus.on('ev:fs', (ev) => {
    const dirs = ev?.overflow ? [...nodes.keys()].filter((k) => nodes.get(k).dir && nodes.get(k).children) : [...new Set((ev?.changes || []).map((c) => dirname(c.path)))];
    for (const d of dirs) if (nodes.get(d)?.children) { nodes.get(d).children = null; load(d); }
  });

  function collapseAll() {
    for (const n of nodes.values()) if (n.dir && n.path) n.open = false;
    openDirs.clear();
    session.update({ openDirs: [] });
    focusIdx = 0;
    render();
  }

  function reset() {
    nodes.clear();
    nodes.set('', { path: '', name: '', dir: true, open: true, children: null, loading: false });
    render();
    load('');
  }

  load('');

  return {
    reveal,
    collapseAll,
    refresh: reset,
    focus() {
      scroller.focus();
    },
  };
}
