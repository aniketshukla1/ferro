// Editor host: tab strip (preview + pinned tabs), breadcrumbs, the active document view,
// view cache, history and session persistence.
import { h, mount } from '../core/dom.js';
import { api, has } from '../core/api.js';
import { store } from '../core/store.js';
import { bus } from '../core/bus.js';
import { basename, dirname, formatBytes, formatCount, LRU } from '../core/util.js';
import { icon, fileIcon } from '../ui/icons.js';
import { createCodeView, createImageView } from './viewer.js';
import { createMarkdownView, isMarkdown } from './markdown.js';
import { session } from './session.js';

const MAX_TABS = 20;
const MAX_LIVE_VIEWS = 8;

export function createEditor(shell, { home }) {
  /** @type {{path:string, preview:boolean, state?:any}[]} */
  let tabs = [];
  let active = null;
  const closedStack = [];
  const views = new Map(); // path -> { view, meta }
  const liveOrder = new LRU(MAX_LIVE_VIEWS + 4);
  const back = [];
  const fwd = [];
  let gitMap = new Map();

  const homeView = h('div', { class: 'view home' }, home.el);
  const docView = h('div', { class: 'view doc', hidden: true });
  mount(shell.viewsEl, homeView, docView);

  // ---------- tab strip (keyed: tab elements are reused so double-click works) ----------
  const tabEls = new Map();
  function tabEl(path) {
    let el = tabEls.get(path);
    if (el) return el;
    const dir = h('span', { class: 'tab-dir' });
    const dot = h('span', { class: 'dot-changed', title: 'Changed in working tree' });
    el = h('div', {
      class: 'tab',
      role: 'tab',
      title: path,
      on: {
        click: () => { if (active !== path) activate(path); },
        dblclick: () => pin(path),
        auxclick: (e) => { if (e.button === 1) close(path); },
        keydown: (e) => { if (e.key === 'Enter') activate(path, { focus: true }); },
      },
    },
    fileIcon(path, 'sm'),
    h('span', { class: 'tab-name' }, basename(path)),
    dir,
    dot,
    h('button', {
      class: 'tab-close',
      tabindex: '-1',
      'aria-label': `Close ${basename(path)}`,
      on: { click: (e) => { e.stopPropagation(); close(path); } },
    }, icon('x', 'xs')));
    el.__dir = dir;
    el.__dot = dot;
    tabEls.set(path, el);
    return el;
  }

  function renderStrip() {
    const names = new Map();
    for (const t of tabs) names.set(basename(t.path), (names.get(basename(t.path)) || 0) + 1);
    for (const p of [...tabEls.keys()]) if (!tabs.some((t) => t.path === p)) tabEls.delete(p);
    const els = tabs.map((t) => {
      const el = tabEl(t.path);
      const on = t.path === active;
      el.classList.toggle('preview', t.preview);
      el.setAttribute('aria-selected', String(on));
      el.tabIndex = on ? 0 : -1;
      const dup = names.get(basename(t.path)) > 1;
      el.__dir.hidden = !dup;
      if (dup) el.__dir.textContent = basename(dirname(t.path)) || '/';
      el.__dot.hidden = !gitMap.get(t.path);
      return el;
    });
    const same = els.length === shell.tabsEl.children.length && els.every((el, i) => shell.tabsEl.children[i] === el);
    if (!same) shell.tabsEl.replaceChildren(...els);
    shell.tabsEl.querySelector('[aria-selected="true"]')?.scrollIntoView({ block: 'nearest', inline: 'nearest' });
  }

  // ---------- breadcrumbs ----------
  function renderCrumbs(path, meta, view) {
    if (!path) { mount(shell.crumbsEl); return; }
    const parts = path.split('/');
    const segs = [];
    parts.forEach((p, i) => {
      if (i) segs.push(h('span', { class: 'sep' }, icon('chevron-right', 'xs')));
      const full = parts.slice(0, i + 1).join('/');
      const last = i === parts.length - 1;
      segs.push(h('button', {
        class: `crumb${last ? ' last' : ''}`,
        on: { click: () => bus.emit('tree:reveal', { path: full, focus: true }) },
      }, last ? fileIcon(path, 'xs') : null, p));
    });
    const info = [];
    if (meta) {
      if (meta.language) info.push(meta.language);
      if (meta.kind === 'text') info.push(`${formatCount(meta.lines)} lines`);
      info.push(formatBytes(meta.size));
    }
    mount(shell.crumbsEl, segs, h('span', { class: 'crumb-meta' }, info.map((t) => h('span', null, t)), view?.toolbar?.() || null));
  }

  // ---------- views ----------
  async function viewFor(path) {
    const cached = views.get(path);
    if (cached) return cached;
    const meta = await api.file(path);
    let view;
    if (meta.kind === 'image') view = createImageView(path, meta);
    else if (meta.kind === 'binary') view = binaryView(path, meta);
    else if (isMarkdown(meta) && has('markdown.v2') && !meta.tooLarge) {
      view = createMarkdownView(path, meta, { onOpen: (p, o) => open(p, o), preferSource: store.get('settings')?.['ui.markdownPreview'] === false });
    } else view = createCodeView(path, meta);
    const tab = tabs.find((t) => t.path === path);
    if (tab?.state) view.restore(tab.state);
    const entry = { view, meta };
    views.set(path, entry);
    trimViews(path);
    return entry;
  }

  function trimViews(keep) {
    liveOrder.set(keep, true);
    if (views.size <= MAX_LIVE_VIEWS) return;
    const recent = new Set([...liveOrder.keys()].slice(-MAX_LIVE_VIEWS));
    for (const p of [...views.keys()]) {
      if (views.size <= MAX_LIVE_VIEWS) break;
      if (p === keep || p === active || recent.has(p)) continue;
      dropView(p);
    }
  }

  function dropView(path) {
    const v = views.get(path);
    if (!v) return;
    const tab = tabs.find((t) => t.path === path);
    if (tab) tab.state = v.view.state();
    v.view.destroy();
    v.view.el.remove();
    views.delete(path);
  }

  function binaryView(path, meta) {
    const el = h('div', { class: 'view-center' },
      h('div', { class: 'empty' }, icon('file', 'xl'),
        h('h3', null, 'Binary file'),
        h('p', null, `${formatBytes(meta.size)} · not shown as text`),
        h('a', { class: 'btn sm', href: api.rawUrl(path), target: '_blank', rel: 'noopener noreferrer' }, icon('external', 'sm'), 'Open raw')));
    return { el, kind: 'binary', focus() {}, state: () => null, restore() {}, onShow() {}, destroy() {} };
  }

  function errorView(path, err) {
    return h('div', { class: 'view-center' },
      h('div', { class: 'empty' }, icon('alert', 'xl'),
        h('h3', null, err.status === 404 ? 'File not found' : 'Cannot open file'),
        h('p', null, err.message || String(err)),
        h('button', { class: 'btn sm', on: { click: () => activate(path, { focus: true }) } }, icon('refresh', 'sm'), 'Retry')));
  }

  function loadingView() {
    const rows = [];
    for (let i = 0; i < 18; i++) {
      const bar = h('span', { class: 'skel' });
      bar.style.width = `${90 + ((i * 131) % 420)}px`;
      rows.push(h('div', { class: 'row' }, bar));
    }
    const el = h('div', { class: 'loading-doc' }, rows);
    return el;
  }

  // ---------- public ops ----------
  let token = 0;
  async function activate(path, { focus = false, line, pushHistory = true } = {}) {
    if (!path) return showHome();
    const my = ++token;
    if (active && active !== path && pushHistory) {
      back.push({ path: active, line: views.get(active)?.view.line });
      if (back.length > 50) back.shift();
      fwd.length = 0;
    }
    const prev = active;
    if (prev && views.has(prev)) {
      const t = tabs.find((x) => x.path === prev);
      if (t) t.state = views.get(prev).view.state();
    }
    active = path;
    store.set('active', path);
    homeView.hidden = true;
    docView.hidden = false;
    renderStrip();
    persist();
    let entry = views.get(path);
    if (!entry) {
      mount(docView, loadingView());
      renderCrumbs(path, null);
      try {
        entry = await viewFor(path);
      } catch (e) {
        if (my === token) mount(docView, errorView(path, e));
        return;
      }
      if (my !== token) return;
    }
    if (docView.firstChild !== entry.view.el) mount(docView, entry.view.el);
    renderCrumbs(path, entry.meta, entry.view);
    entry.view.onShow?.();
    if (line) entry.view.gotoLine?.(line);
    if (focus) entry.view.focus();
    liveOrder.set(path, true);
  }

  /**
   * Open a file. preview=true reuses the single preview tab.
   * @param {string} path
   * @param {{preview?:boolean, focus?:boolean, line?:number}} [o]
   */
  function open(path, { preview = false, focus = true, line } = {}) {
    let tab = tabs.find((t) => t.path === path);
    if (tab) {
      if (!preview && tab.preview) tab.preview = false;
    } else {
      tab = { path, preview };
      const pi = preview ? tabs.findIndex((t) => t.preview) : -1;
      if (pi >= 0) {
        dropView(tabs[pi].path);
        tabs[pi] = tab;
      } else {
        const ai = tabs.findIndex((t) => t.path === active);
        tabs.splice(ai >= 0 ? ai + 1 : tabs.length, 0, tab);
        while (tabs.length > MAX_TABS) {
          const victim = tabs.findIndex((t) => t.path !== path && t.path !== active);
          if (victim < 0) break;
          dropView(tabs[victim].path);
          tabs.splice(victim, 1);
        }
      }
    }
    session.touchRecent(path);
    return activate(path, { focus, line });
  }

  function pin(path) {
    const t = tabs.find((x) => x.path === path);
    if (t && t.preview) {
      t.preview = false;
      renderStrip();
      persist();
    }
  }

  function close(path = active) {
    const i = tabs.findIndex((t) => t.path === path);
    if (i < 0) return;
    closedStack.push(path);
    if (closedStack.length > 30) closedStack.shift();
    tabs.splice(i, 1);
    dropView(path);
    if (path === active) {
      const next = tabs[i] || tabs[i - 1];
      if (next) activate(next.path, { focus: true, pushHistory: false });
      else showHome();
    } else renderStrip();
    persist();
  }

  /** Rebuild every view (e.g. after the code font size changed row heights). */
  function resetViews() {
    const cur = active;
    for (const p of [...views.keys()]) dropView(p);
    if (cur) activate(cur, { pushHistory: false });
  }

  function showHome() {
    if (active && views.has(active)) {
      const t = tabs.find((x) => x.path === active);
      if (t) t.state = views.get(active).view.state();
    }
    active = null;
    store.set('active', null);
    store.set('cursor', null);
    docView.hidden = true;
    homeView.hidden = false;
    renderStrip();
    renderCrumbs(null);
    home.refresh?.();
    persist();
  }

  function cycle(dir) {
    if (!tabs.length) return;
    const i = tabs.findIndex((t) => t.path === active);
    const next = tabs[(i + dir + tabs.length) % tabs.length];
    activate(next.path, { focus: true });
  }

  function goHistory(dir) {
    const from = dir < 0 ? back : fwd;
    const to = dir < 0 ? fwd : back;
    const dest = from.pop();
    if (!dest) return;
    if (active) to.push({ path: active, line: views.get(active)?.view.line });
    if (!tabs.some((t) => t.path === dest.path)) tabs.push({ path: dest.path, preview: false });
    activate(dest.path, { focus: true, line: dest.line, pushHistory: false });
  }

  function persist() {
    session.update({
      tabs: tabs.map((t) => ({ path: t.path, preview: t.preview, state: t.path === active ? views.get(t.path)?.view.state() ?? t.state : t.state })),
      active,
    });
  }

  function restore(data) {
    tabs = (data?.tabs || []).filter((t) => t && typeof t.path === 'string').map((t) => ({ path: t.path, preview: !!t.preview, state: t.state }));
    if (data?.active && tabs.some((t) => t.path === data.active)) activate(data.active, { pushHistory: false });
    else showHome();
  }

  store.subscribe('git', (g) => {
    gitMap = new Map((g?.files || []).map((f) => [f.path, f.untracked ? '?' : f.worktree || f.index]));
    renderStrip();
  });
  bus.on('view:state', (path) => { if (path === active) persist(); });
  bus.on('ev:workspace', () => {
    for (const p of [...views.keys()]) dropView(p);
    tabs = [];
    showHome();
  });

  return {
    open,
    close,
    pin,
    activate,
    resetViews,
    showHome,
    restore,
    next: () => cycle(1),
    prev: () => cycle(-1),
    back: () => goHistory(-1),
    forward: () => goHistory(1),
    reopenClosed() {
      const p = closedStack.pop();
      if (p) open(p);
    },
    selectTab(n) {
      const t = tabs[n];
      if (t) activate(t.path, { focus: true });
    },
    get active() { return active; },
    get tabs() { return tabs; },
    activeView() { return active ? views.get(active)?.view || null : null; },
    activeMeta() { return active ? views.get(active)?.meta || null : null; },
  };
}
