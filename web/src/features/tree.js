import { h, mount } from '../core/dom.js';
import { api } from '../core/api.js';
import { store } from '../core/store.js';
import { bus } from '../core/bus.js';
import { VirtualList } from '../core/virtual.js';
import { execute } from '../core/commands.js';
import { basename, dirname } from '../core/util.js';
import { icon, fileIcon, folderIcon } from '../ui/icons.js';
import { session } from './session.js';

export function revealDir(path) {
  execute('panel.files');
  bus.emit('tree:reveal', { path });
}

const rowHeight = () => parseFloat(getComputedStyle(document.documentElement).getPropertyValue('--h-row')) || 26;

export function createTree(container, { onOpen, onOpenState }) {
  const ROW_H = rowHeight();
  const nodes = new Map();
  nodes.set('', { path: '', name: '', dir: true, open: true, children: null, loading: false });
  let rows = [];
  let focusIdx = 0;
  let kbd = false;
  let current = null;
  let gitMap = new Map();
  let dirtyDirs = new Set();
  let deletedByDir = new Map(); // dir -> git-deleted file paths directly inside it
  const openDirs = new Set(session.data.openDirs || []);
  let showIgnored = false;
  let filterText = '';

  const scroller = h('div', { class: 'tree', role: 'tree', tabindex: '0', 'aria-label': 'Files' });
  const skeleton = h('div', { class: 'tree-skel' }, [70, 55, 82, 48, 64, 58, 76].map((w) => {
    const s = h('span', { class: 'skel' });
    s.style.width = `${w}%`;
    return s;
  }));
  const filterInput = h('input', {
    class: 'tree-filter',
    type: 'search',
    placeholder: 'Filter files…',
    'aria-label': 'Filter files',
    on: {
      input: (e) => {
        filterText = e.target.value.trim().toLowerCase();
        render();
      },
      keydown: (e) => {
        if (e.key === 'Escape') {
          e.stopPropagation();
          filterInput.value = '';
          filterText = '';
          render();
          scroller.focus();
        }
      },
    },
  });
  const filterWrap = h('div', { class: 'tree-filter-wrap' }, filterInput);
  mount(container, skeleton, filterWrap, scroller);

  const vl = new VirtualList({ scroller, rowHeight: ROW_H, overscan: 16, create: createRow, update: updateRow });

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
        if (node.children.length === 1 && nodes.get(node.children[0]).dir && !nodes.get(node.children[0]).ignored) {
          const c = nodes.get(node.children[0]);
          if (node.open) { c.open = true; openDirs.add(c.path); }
          await load(c.path);
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

  /** Ancestors shown in the same compact row as `n`. */
  function chainAbove(n) {
    const out = [];
    for (let cur = n, pn; !cur.ignored && (pn = nodes.get(dirname(cur.path))) && pn.path && !pn.ignored && pn.children?.length === 1; cur = pn) out.push(pn);
    return out;
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
      // Deleted files are gone from disk: show them (struck through) where they lived.
      const gone = node?.children ? (deletedByDir.get(dirPath) || []).filter((p) => !nodes.get(p) || nodes.get(p).deleted) : [];
      for (const p of gone) nodes.set(p, { path: p, name: basename(p), deleted: true });
      for (const p of gone.length ? [...node.children, ...gone].sort((a, b) => (nodes.get(b).dir || 0) - (nodes.get(a).dir || 0) || (a < b ? -1 : 1)) : node?.children || []) {
        const child = nodes.get(p);
        if (!showIgnored && child?.ignored) continue;
        const { tail, label } = chainTail(child);
        if (filterText) {
          const matchesSelf = tail.path.toLowerCase().includes(filterText) || label.toLowerCase().includes(filterText);
          const hasDesc = (d) => {
            for (const cp of nodes.get(d)?.children || []) {
              const cn = nodes.get(cp);
              if (!showIgnored && cn?.ignored) continue;
              if (cn?.path.toLowerCase().includes(filterText) || cn?.name?.toLowerCase().includes(filterText)) return true;
              if (cn?.dir && hasDesc(cn.path)) return true;
            }
            return false;
          };
          if (!matchesSelf && tail.dir && !hasDesc(tail.path)) continue;
          if (!matchesSelf && !tail.dir) continue;
        }
        out.push({ node: tail, label, depth });
        if (tail.dir && (filterText ? tail.children : (tail.open && tail.children))) {
          walk(tail.path, depth + 1);
        }
      }
    };
    walk('', 0);
    return out;
  }

  let renderQueued = false;
  let lastOpen = null;
  const anyOpen = () => [...nodes.values()].some((n) => n.dir && n.path && n.open && (showIgnored || !n.ignored));

  function render() {
    if (renderQueued) return;
    renderQueued = true;
    queueMicrotask(() => {
      renderQueued = false;
      rows = flatten();
      skeleton.hidden = nodes.get('').children !== null;
      // The toolbar's single Expand / Collapse button follows whether any folder is open.
      const open = anyOpen();
      if (open !== lastOpen) { lastOpen = open; onOpenState?.(open); }
      focusIdx = Math.min(focusIdx, Math.max(0, rows.length - 1));
      vl.setCount(rows.length);
      vl.refresh();
    });
  }

  function setOpen(node, open) {
    node.open = open;
    if (open) {
      openDirs.add(node.path);
      let cur = node;
      while (cur.children?.length === 1) {
        const only = nodes.get(cur.children[0]);
        if (!only?.dir || only.ignored) break;
        only.open = true; openDirs.add(only.path); cur = only;
      }
    } else {
      openDirs.delete(node.path);
      // A compact row (a/b/c) collapses as one; its head's parent stays as the user left it.
      for (const p of chainAbove(node)) {
        p.open = false;
        openDirs.delete(p.path);
      }
    }
    session.update({ openDirs: [...openDirs] });
    if (open && !node.children) load(node.path);
    render();
  }

  // A deleted file only exists in git: show what was removed.
  const openNode = (n, o) => (n.deleted ? bus.emit('diff:open', { path: n.path }) : onOpen(n.path, o));

  function onRowClick(i, e) {
    const r = rows[i];
    if (!r) return;
    focusIdx = i;
    setKbd(false);
    const n = r.node;
    if (n.dir) setOpen(n, !n.open);
    else openNode(n, { preview: !(e.metaKey || e.ctrlKey), focus: false });
    vl.refresh();
  }

  function onRowDblClick(i) {
    const n = rows[i]?.node;
    if (n && !n.dir) openNode(n, { preview: false, focus: true });
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
        else if (n) openNode(n, { preview: false, focus: true });
        break;
      case ' ':
        if (n && !n.dir) openNode(n, { preview: true, focus: false });
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
    deletedByDir = new Map();
    for (const f of g?.files || []) {
      const code = f.conflicted ? 'U' : f.untracked ? '?' : (f.worktree || f.index || 'M');
      gitMap.set(f.path, code);
      if (code === 'D' && !f.dir) deletedByDir.set(dirname(f.path), [...(deletedByDir.get(dirname(f.path)) || []), f.path]);
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

  let expanding = false;
  function collapseAll() {
    expanding = false;
    for (const n of nodes.values()) if (n.dir && n.path) n.open = false;
    openDirs.clear();
    session.update({ openDirs: [] });
    focusIdx = 0;
    render();
  }

  async function expandAll() {
    expanding = true;
    const markOpen = () => {
      for (const n of nodes.values()) {
        if (n.dir && n.path && (showIgnored || !n.ignored)) { n.open = true; openDirs.add(n.path); }
      }
    };
    markOpen();
    let pending = [...nodes.values()].filter((n) => n.dir && !n.children && !n.loading && (showIgnored || !n.ignored));
    while (pending.length && expanding) {
      await Promise.all(pending.map((n) => load(n.path)));
      if (!expanding) return;
      markOpen();
      pending = [...nodes.values()].filter((n) => n.dir && !n.children && !n.loading && (showIgnored || !n.ignored));
    }
    if (!expanding) return;
    session.update({ openDirs: [...openDirs] });
    render();
  }

  function toggleIgnored() {
    showIgnored = !showIgnored;
    render();
    return showIgnored;
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
    expandAll,
    toggleIgnored,
    get showIgnored() { return showIgnored; },
    get anyOpen() { return anyOpen(); },
    setFilter(text) {
      filterInput.value = text;
      filterText = text.trim().toLowerCase();
      render();
    },
    refresh: reset,
    focus() {
      scroller.focus();
    },
  };
}
