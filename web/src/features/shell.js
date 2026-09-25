// App shell: topbar, sidebar (panel switcher + panels), main area, inspector, status bar.
// Features plug in through registerPanel / registerInspectorTab.
import { h, mount } from '../core/dom.js';
import { keysEl } from '../core/keys.js';
import { execute } from '../core/commands.js';
import { store } from '../core/store.js';
import { clamp } from '../core/util.js';
import { icon, brandMark } from '../ui/icons.js';
import { session } from './session.js';
import { isDarkTheme, currentTheme } from './themes.js';

export function buildShell(root) {
  const panels = new Map(); // id -> {def, section, button}
  const inspTabs = new Map();

  // ---------- topbar ----------
  const wsName = h('span', { class: 'ws-name truncate' }, '…');
  const wsBranch = h('span', { class: 'ws-branch' }, icon('git-branch', 'xs'), h('span', { class: 'truncate' }, ''));
  const prSlot = h('div', { class: 'row' });
  const themeBtn = h('button', { class: 'icon-btn', 'aria-label': 'Toggle light and dark', 'data-tip': 'Light / dark', on: { click: () => execute('theme.toggle') } });
  const sbToggle = h('button', { class: 'icon-btn', id: 'sb-toggle', 'aria-label': 'Toggle sidebar', 'data-tip': 'Sidebar', 'data-keys': 'Mod+B', on: { click: () => execute('view.sidebar') } }, icon('panel-left'));
  const topbar = h('header', { class: 'topbar' },
    h('div', { class: 'tb-left' },
      h('button', { class: 'brand', 'aria-label': 'ferro home', 'data-tip': 'Home', on: { click: () => execute('view.home') } }, brandMark()),
      sbToggle,
      h('span', { class: 'tb-divider', 'aria-hidden': 'true' }),
      h('button', { class: 'ws-chip', 'data-tip': 'Go to file', 'data-keys': 'Mod+K', on: { click: () => execute('palette.files') } }, wsName, wsBranch)),
    h('button', {
      class: 'cmdbar',
      'aria-label': 'Search files, symbols and commands',
      on: { click: () => execute('palette.files') },
    }, icon('search', 'sm'), h('span', { class: 'cmdbar-text' }, 'Search files, symbols, text'), keysEl('Mod+K')),
    h('div', { class: 'tb-right' },
      prSlot,
      h('button', { class: 'icon-btn', 'aria-label': 'Ask AI', 'data-tip': 'Ask AI', 'data-keys': 'Mod+I', on: { click: () => execute('ai.ask') } }, icon('sparkles')),
      themeBtn,
      h('button', { class: 'icon-btn', 'aria-label': 'Settings', 'data-tip': 'Settings', 'data-keys': 'Mod+,', on: { click: () => execute('settings.open') } }, icon('settings')),
      h('button', { class: 'icon-btn', id: 'insp-toggle', 'aria-label': 'Toggle inspector', 'data-tip': 'Inspector', 'data-keys': 'Mod+J', on: { click: () => execute('view.inspector') } }, icon('panel-right')),
    ));

  function syncThemeButton() {
    mount(themeBtn, icon(isDarkTheme() ? 'sun' : 'moon'));
  }
  syncThemeButton();
  document.addEventListener('ferro:theme', syncThemeButton);

  // ---------- sidebar / main / inspector ----------
  const switcher = h('div', { class: 'sb-switch', role: 'tablist', 'aria-label': 'Sidebar views' });
  const sidebar = h('aside', { class: 'sidebar', 'aria-label': 'Sidebar' }, switcher);
  const tabsEl = h('div', { class: 'tabs', role: 'tablist', 'aria-label': 'Open files' });
  const crumbsEl = h('div', { class: 'crumbs' });
  const viewsEl = h('div', { class: 'views' });
  const main = h('main', { class: 'main' }, tabsEl, crumbsEl, viewsEl);
  const inspHead = h('div', { class: 'insp-tabs', role: 'tablist', 'aria-label': 'Inspector' });
  const inspBody = h('div', { class: 'insp-body' });
  const inspector = h('aside', { class: 'inspector', 'aria-label': 'Inspector' },
    h('div', { class: 'insp-head' }, inspHead,
      h('button', { class: 'icon-btn sm', 'aria-label': 'Close inspector', 'data-tip': 'Close', 'data-keys': 'Mod+J', on: { click: () => execute('view.inspector') } }, icon('x', 'sm'))),
    inspBody);
  const sbResizer = h('div', { class: 'resizer sb', role: 'separator', 'aria-orientation': 'vertical', 'aria-label': 'Resize sidebar' });
  const ibResizer = h('div', { class: 'resizer ib', role: 'separator', 'aria-orientation': 'vertical', 'aria-label': 'Resize inspector' });
  const body = h('div', { class: 'body' }, sidebar, sbResizer, main, ibResizer, inspector);
  const banners = h('div', { class: 'banners' });
  const statusEl = h('footer', { class: 'statusbar', 'aria-label': 'Status bar' });
  const app = h('div', { class: 'app' }, topbar, banners, body, statusEl);
  mount(root, app);
  root.removeAttribute('aria-busy');

  // ---------- layout state ----------
  function applyLayout() {
    const L = session.data.layout;
    app.classList.toggle('no-sidebar', !L.sidebar);
    app.classList.toggle('no-inspector', !L.inspector);
    app.classList.toggle('force-inspector', !!L.inspector);
    root.style.setProperty('--sidebar-w', `${L.sidebarW}px`);
    root.style.setProperty('--inspector-w', `${L.inspectorW}px`);
    document.getElementById('insp-toggle')?.setAttribute('aria-pressed', String(!!L.inspector));
    sbToggle.setAttribute('aria-pressed', String(!!L.sidebar));
    showPanel(L.panel, { focus: false, persist: false });
  }

  function dragResize(handle, key, dir) {
    handle.addEventListener('pointerdown', (e) => {
      if (e.button !== 0) return;
      e.preventDefault();
      const startX = e.clientX;
      const startW = session.data.layout[key];
      handle.classList.add('dragging');
      document.body.classList.add('resizing');
      handle.setPointerCapture(e.pointerId);
      const move = (ev) => {
        const w = clamp(startW + (ev.clientX - startX) * dir, 200, Math.min(720, window.innerWidth * 0.6));
        root.style.setProperty(key === 'sidebarW' ? '--sidebar-w' : '--inspector-w', `${w}px`);
        session.data.layout[key] = Math.round(w);
      };
      const up = () => {
        handle.classList.remove('dragging');
        document.body.classList.remove('resizing');
        handle.removeEventListener('pointermove', move);
        handle.removeEventListener('pointerup', up);
        session.layout({});
      };
      handle.addEventListener('pointermove', move);
      handle.addEventListener('pointerup', up);
    });
    handle.addEventListener('dblclick', () => {
      session.layout({ [key]: key === 'sidebarW' ? 280 : 360 });
      applyLayout();
    });
  }
  dragResize(sbResizer, 'sidebarW', 1);
  dragResize(ibResizer, 'inspectorW', -1);

  // ---------- panels ----------
  let activePanel = null;
  function registerPanel(def) {
    const section = h('section', { class: 'panel', 'data-panel': def.id, hidden: true, 'aria-label': def.title });
    const button = h('button', {
      class: 'sw-btn',
      role: 'tab',
      'aria-selected': 'false',
      'aria-label': def.title,
      'data-tip': def.title,
      'data-keys': def.keys?.[0],
      on: { click: () => showPanel(def.id) },
    }, icon(def.icon), h('span', { class: 'sw-label' }, def.label || def.title));
    switcher.appendChild(button);
    sidebar.appendChild(section);
    panels.set(def.id, { def, section, button, rendered: false });
    if (def.id === session.data.layout.panel) applyLayout();
    return { section, button, setBadge: (n) => setBadge(button, n) };
  }

  function setBadge(button, n) {
    let b = button.querySelector('.badge');
    if (!n) { b?.remove(); return; }
    if (!b) { b = h('span', { class: 'badge' }); button.appendChild(b); }
    b.textContent = n > 999 ? '999+' : String(n);
  }

  function showPanel(id, { focus = true, persist = true } = {}) {
    const p = panels.get(id) || panels.values().next().value;
    if (!p) return;
    for (const [pid, x] of panels) {
      const on = pid === p.def.id;
      x.section.hidden = !on;
      x.button.setAttribute('aria-selected', String(on));
    }
    if (!p.rendered) {
      p.rendered = true;
      p.def.render(p.section);
    }
    activePanel = p.def.id;
    if (persist) {
      session.layout({ panel: activePanel, sidebar: true });
      app.classList.remove('no-sidebar');
      sbToggle.setAttribute('aria-pressed', 'true');
    }
    p.def.onShow?.({ focus });
  }

  function togglePanel(id) {
    const L = session.data.layout;
    if (L.sidebar && activePanel === id) {
      session.layout({ sidebar: false });
      applyLayout();
      return;
    }
    session.layout({ sidebar: true, panel: id });
    showPanel(id);
    applyLayout();
  }

  function toggleSidebar() {
    session.layout({ sidebar: !session.data.layout.sidebar });
    applyLayout();
  }

  function toggleInspector(force) {
    const on = typeof force === 'boolean' ? force : !session.data.layout.inspector;
    session.layout({ inspector: on });
    applyLayout();
    if (on && !activeInsp) inspTabs.values().next().value?.show();
  }

  // ---------- inspector tabs ----------
  let activeInsp = null;
  function registerInspectorTab(def) {
    const btn = h('button', { class: 'insp-tab', role: 'tab', 'aria-selected': 'false', on: { click: () => show() } }, icon(def.icon, 'sm'), def.title);
    inspHead.appendChild(btn);
    const content = h('div', { class: 'insp-pane', hidden: true });
    inspBody.appendChild(content);
    let rendered = false;
    function show() {
      for (const t of inspTabs.values()) {
        t.btn.setAttribute('aria-selected', 'false');
        t.content.hidden = true;
      }
      btn.setAttribute('aria-selected', 'true');
      content.hidden = false;
      if (!rendered) { rendered = true; def.render(content); }
      def.onShow?.();
      activeInsp = def.id;
    }
    inspTabs.set(def.id, { btn, content, show });
    if (!activeInsp) show();
    return { show };
  }

  // ---------- workspace chip ----------
  store.subscribe('meta', (m) => {
    if (!m) return;
    wsName.textContent = m.workspace?.name || 'workspace';
    document.title = `${m.workspace?.name || 'ferro'} · ferro`;
    const b = m.workspace?.branch;
    if (b && !store.get('git')) { wsBranch.lastChild.textContent = b; wsBranch.hidden = false; }
  });
  store.subscribe('git', (g) => {
    const name = g?.branch || (g?.detached ? 'detached' : store.get('meta')?.workspace?.branch || '');
    wsBranch.lastChild.textContent = name;
    wsBranch.hidden = !name;
  });

  return {
    app, root, topbar, sidebar, main, tabsEl, crumbsEl, viewsEl, inspector, banners, statusEl, prSlot,
    registerPanel, showPanel, togglePanel, toggleSidebar, toggleInspector, registerInspectorTab, applyLayout,
    get activePanel() { return activePanel; },
    get themeId() { return currentTheme(); },
    focusMain() { viewsEl.querySelector('.view:not([hidden]) [tabindex="0"]')?.focus(); },
  };
}
