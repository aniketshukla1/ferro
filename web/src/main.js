// ferro frontend entry: boot, feature wiring, commands and shortcuts.
// Boot graph: shell, tree, tabs, code viewer, home, status bar. Everything else loads on first
// use and is warmed up while the browser is idle (FRONTEND.md § 3.1).
import { h, mount } from './core/dom.js';
import { api, has } from './core/api.js';
import { store } from './core/store.js';
import { bus } from './core/bus.js';
import { connectEvents } from './core/sse.js';
import { installKeymap, setHostGetter, bindKey } from './core/keys.js';
import { command, execute } from './core/commands.js';
import { debounce, lazy, whenIdle } from './core/util.js';
import { installTooltips, toast } from './ui/overlay.js';
import { icon } from './ui/icons.js';
import { buildShell } from './features/shell.js';
import { session } from './features/session.js';
import { applySettingsTheme, cycleTheme, toggleLightDark } from './features/themes.js';
import { applyUiSettings } from './features/prefs.js';
import { createHome } from './features/home.js';
import { createEditor } from './features/editor.js';
import { createTree } from './features/tree.js';
import { createStatusBar } from './features/status.js';
import { watchConnection } from './features/connection.js';

// On-demand modules (same URL as the static import would use, so the module map dedupes them).
const load = {
  palette: lazy(() => import('./features/palette.js')),
  panels: lazy(() => import('./features/panels.js')),
  find: lazy(() => import('./features/find.js')),
  settings: lazy(() => import('./features/settings.js')),
  chrome: lazy(() => import('./features/chrome.js')),
  compat: lazy(() => import('./features/compat.js')),
  markdown: lazy(() => import('./features/markdown.js')),
};

async function boot() {
  performance.mark('ferro:boot');
  const root = document.getElementById('app');
  const params = new URLSearchParams(location.search);
  const mockMode = params.has('mock') ? (params.get('mock') || '1') : null;
  if (mockMode) {
    const { installMock } = await import('./mock/index.js');
    installMock(mockMode);
  }
  installKeymap();
  installTooltips();
  setHostGetter(() => store.get('meta')?.host || 'browser');

  let meta;
  let settings;
  let saved;
  try {
    [meta, settings, saved] = await Promise.all([
      api.meta(),
      api.settings().catch(() => ({ values: {} })),
      api.session().catch(() => ({ data: null })),
    ]);
  } catch (e) {
    root.removeAttribute('aria-busy');
    if (e.status === 401) (await load.chrome()).showAuthScreen(root);
    else showOffline(root, e);
    return;
  }

  session.load(saved?.data);
  store.set('meta', meta);
  store.set('index', meta.index);
  store.set('settings', settings.values || {});
  applySettingsTheme(settings.values);
  applyUiSettings(settings.values);

  const autoReveal = () => store.get('settings')?.['ui.autoReveal'] !== false;
  const shell = buildShell(root);
  performance.mark('ferro:shell');
  const home = createHome({ onOpen: (p, o) => editor.open(p, o) });
  const editor = createEditor(shell, { home });
  // On narrow screens the sidebar overlays the editor: close it once a file is chosen.
  const narrow = window.matchMedia('(max-width: 760px)');
  const onOpen = (path, o) => {
    const r = editor.open(path, o);
    if (narrow.matches && session.data.layout.sidebar) shell.toggleSidebar();
    return r;
  };

  // ---------- sidebar panels ----------
  let tree = null;
  shell.registerPanel({
    id: 'files',
    title: 'Files',
    icon: 'files',
    keys: ['Mod+Shift+E'],
    render(section) {
      const body = h('div', { class: 'panel-body' });
      mount(section,
        h('div', { class: 'panel-head' },
          h('span', { class: 'panel-title truncate' }, meta.workspace?.name || 'Files'),
          h('div', { class: 'panel-actions' },
            h('button', { class: 'icon-btn sm', 'aria-label': 'Reveal active file', 'data-tip': 'Reveal active file', on: { click: () => execute('file.reveal') } }, icon('eye', 'sm')),
            h('button', { class: 'icon-btn sm', 'aria-label': 'Collapse all', 'data-tip': 'Collapse all', on: { click: () => tree?.collapseAll() } }, icon('collapse-all', 'sm')),
            h('button', { class: 'icon-btn sm', 'aria-label': 'Refresh', 'data-tip': 'Refresh', on: { click: () => execute('index.rebuild') } }, icon('refresh', 'sm')))),
        body);
      tree = createTree(body, { onOpen });
      const active = store.get('active');
      if (active && autoReveal()) bus.emit('tree:reveal', { path: active, align: 'center' });
    },
    onShow: ({ focus }) => { if (focus) tree?.focus(); },
  });
  let search = null;
  let outline = null;
  const changesPanel = shell.registerPanel({ id: 'changes', title: 'Changes', icon: 'git-compare', keys: ['Mod+Shift+G'], render: async (s) => { (await load.panels()).renderChangesPanel(s, { onOpen }); } });
  shell.registerPanel({ id: 'search', title: 'Search', icon: 'search', keys: ['Mod+Shift+F'], render: async (s) => { search = (await load.panels()).renderSearchPanel(s, { onOpen }); }, onShow: ({ focus }) => { if (focus) search?.focus(); } });
  shell.registerPanel({ id: 'outline', title: 'Outline', icon: 'list-tree', render: async (s) => { outline = (await load.panels()).renderOutlinePanel(s, { editor }); }, onShow: ({ focus }) => { if (focus) outline?.focus(); } });
  store.subscribe('git', (g) => changesPanel.setBadge(g ? g.counts.staged + g.counts.unstaged + g.counts.untracked : 0));

  // ---------- inspector (tabs render the first time the inspector is shown) ----------
  const aiTab = shell.registerInspectorTab({ id: 'ai', title: 'AI', icon: 'sparkles', render: async (el) => (await load.chrome()).renderAiTab(el) });
  shell.registerInspectorTab({ id: 'info', title: 'Info', icon: 'info', render: async (el) => (await load.chrome()).renderInfoTab(el) });

  // Palette and find are created on first use; these proxies keep call sites synchronous-looking.
  const getPalette = lazy(async () => (await load.palette()).createPalette({ editor, onOpen }));
  const palette = { open: (...a) => getPalette().then((p) => p.open(...a)) };
  const getFind = lazy(async () => (await load.find()).createFind({ editor, host: shell.viewsEl }));
  const find = { open: () => getFind().then((f) => f.open()), next: () => getFind().then((f) => f.next()), prev: () => getFind().then((f) => f.prev()) };

  createStatusBar(shell.statusEl, { editor, mockMode });
  watchConnection(shell.banners);
  bus.on('toast', (t) => toast(t));

  registerCommands({ shell, editor, palette, find, getTree: () => tree, getSearch: () => search, aiTab });

  // Narrow screens start with the overlay sidebar closed (in memory only; the saved layout is untouched).
  if (narrow.matches) session.data.layout.sidebar = false;
  shell.applyLayout();
  editor.restore(session.data);
  performance.mark('ferro:ready');

  // Settings changed here or in another window: apply presentation keys live.
  let codeFs = store.get('settings')?.['ui.codeFontSize'];
  store.subscribe('settings', (v) => {
    applySettingsTheme(v);
    applyUiSettings(v);
    if (v?.['ui.codeFontSize'] !== codeFs) { codeFs = v?.['ui.codeFontSize']; editor.resetViews(); }
  });

  // Keep the tree on the active file (only scrolls when the row is off-screen). ui.autoReveal: false opts out.
  store.subscribe('active', (p) => {
    if (p && autoReveal()) bus.emit('tree:reveal', { path: p, align: 'auto' });
  });

  // `ferro file:line` or ?path=&line= deep link
  const initial = meta.initial || (params.get('path') ? { path: params.get('path'), line: Number(params.get('line')) || undefined } : null);
  if (initial?.path) {
    editor.open(initial.path, { line: initial.line, focus: true });
    const clean = new URL(location.href);
    clean.searchParams.delete('path');
    clean.searchParams.delete('line');
    history.replaceState(null, '', clean);
  }

  connectEvents({ metrics: true });
  // Git status: the v1 endpoint once B3 ships, the legacy porcelain text until then (refreshed on fs events).
  const refreshGit = () => (has('git.status.v2') ? api.gitStatus() : load.compat().then((m) => m.compatGitStatus()))
    .then((g) => store.set('git', g)).catch(() => {});
  if (has('git.status.v2') || meta.workspace?.git) {
    refreshGit();
    if (!has('git.status.v2')) bus.on('ev:fs', debounce(refreshGit, 600));
  }
  bus.on('git:refresh', refreshGit);
  // A switched workspace (a PR opened, another folder): fresh meta and status for the new root
  // (the tree and editor reset themselves on the same event).
  bus.on('ev:workspace', () => {
    api.meta().then((m) => store.set('meta', m)).catch(() => {});
    refreshGit();
  });
  if (has('metrics')) api.metrics().then((m) => store.set('metrics', m)).catch(() => {});
  if (mockMode) document.documentElement.classList.add('mock');

  // Warm the on-demand modules once the first screen has settled.
  whenIdle(() => { for (const f of Object.values(load)) f().catch(() => {}); });
}

function showOffline(root, e) {
  mount(root, h('div', { class: 'auth-screen' }, h('div', { class: 'auth-card' },
    icon('wifi-off', 'xl'),
    h('h1', null, 'Cannot reach the ferro server'),
    h('p', { class: 'muted' }, e.message || 'The server is not responding.'),
    h('p', { class: 'faint small' }, 'Start it with ', h('code', null, 'ferro .'), ' — or preview the UI with ', h('code', null, '?mock=1'), '.'),
    h('div', { class: 'row' }, h('button', { class: 'btn primary', on: { click: () => location.reload() } }, icon('refresh', 'sm'), 'Retry')))));
}

function registerCommands({ shell, editor, palette, find, getTree, getSearch, aiTab }) {
  const hasFile = () => !!editor.active;
  const codeView = () => editor.activeView();
  // Find works on source text; in the Markdown preview the browser's own find stays available.
  const findable = () => { const v = codeView(); return v?.kind === 'code' || (v?.kind === 'markdown' && v.mode === 'source'); };
  command({ id: 'find.open', title: 'Find in File', category: 'File', icon: 'search', keys: ['Mod+F'], when: findable, run: () => find.open() });
  command({ id: 'find.next', title: 'Find Next', category: 'File', icon: 'arrow-down', keys: ['F3'], when: findable, run: () => find.next() });
  command({ id: 'find.prev', title: 'Find Previous', category: 'File', icon: 'arrow-up', keys: ['Shift+F3'], when: findable, run: () => find.prev() });

  // Go
  command({ id: 'palette.files', title: 'Go to File…', category: 'Go', icon: 'search', keys: ['Mod+K', 'Mod+P'], inInput: true, run: () => palette.open('') });
  command({ id: 'palette.commands', title: 'Show All Commands', category: 'Go', icon: 'command', keys: ['Mod+Shift+P'], inInput: true, run: (q) => palette.open(`>${typeof q === 'string' ? q : ''}`) });
  command({ id: 'palette.symbols', title: 'Go to Symbol in File…', category: 'Go', icon: 'at', keys: ['Mod+Shift+O'], run: () => palette.open('@') });
  command({ id: 'palette.line', title: 'Go to Line…', category: 'Go', icon: 'enter', keys: ['Mod+G'], run: () => palette.open(':') });
  command({
    id: 'palette.search', title: 'Search in Files', category: 'Go', icon: 'search', keys: ['Mod+Shift+F'], inInput: true,
    run: async () => {
      const q = window.getSelection()?.toString().trim().split('\n')[0] || ''; // read before the panel may load
      await shell.showPanel('search');
      getSearch()?.focus(q);
    },
  });
  command({ id: 'nav.back', title: 'Go Back', category: 'Go', icon: 'chevron-left', keys: ['Alt+ArrowLeft'], run: () => editor.back() });
  command({ id: 'nav.forward', title: 'Go Forward', category: 'Go', icon: 'chevron-right', keys: ['Alt+ArrowRight'], run: () => editor.forward() });
  command({ id: 'view.home', title: 'Go Home', category: 'Go', icon: 'panel-left', run: () => editor.showHome() });

  // View
  command({ id: 'view.sidebar', title: 'Toggle Sidebar', category: 'View', icon: 'panel-left', keys: ['Mod+B'], run: () => shell.toggleSidebar() });
  command({ id: 'view.inspector', title: 'Toggle Inspector', category: 'View', icon: 'panel-right', keys: ['Mod+J'], run: () => shell.toggleInspector() });
  command({ id: 'panel.files', title: 'Show Files', category: 'View', icon: 'files', keys: ['Mod+Shift+E'], inInput: true, run: () => shell.showPanel('files') });
  command({ id: 'panel.changes', title: 'Show Changes', category: 'View', icon: 'git-compare', keys: ['Mod+Shift+G'], inInput: true, run: () => shell.showPanel('changes') });
  command({ id: 'panel.outline', title: 'Show Outline', category: 'View', icon: 'list-tree', run: () => shell.showPanel('outline') });
  command({ id: 'theme.pick', title: 'Color Theme…', category: 'Preferences', icon: 'contrast', run: () => palette.open('', { special: 'theme' }) });
  command({ id: 'theme.cycle', title: 'Next Color Theme', category: 'Preferences', icon: 'contrast', run: () => { const t = cycleTheme(); toast({ title: `Theme: ${t.name}`, timeout: 1500 }); } });
  command({ id: 'theme.toggle', title: 'Toggle Light / Dark', category: 'Preferences', icon: 'contrast', keys: ['Alt+Shift+L'], run: () => toggleLightDark() });
  command({ id: 'settings.open', title: 'Open Settings', category: 'Preferences', icon: 'settings', keys: ['Mod+,'], run: () => load.settings().then((m) => m.openSettings()) });
  const help = () => load.chrome().then((m) => m.showKeyboardHelp());
  command({ id: 'help.keys', title: 'Keyboard Shortcuts', category: 'Help', icon: 'keyboard', keys: ['Mod+/'], run: help });
  bindKey('?', help);
  command({ id: 'ai.ask', title: 'Ask AI', category: 'AI', icon: 'sparkles', keys: ['Mod+I'], run: () => { shell.toggleInspector(true); aiTab.show(); } });

  // Security: sign-out is immediate for this browser; "all browsers" goes through
  // Settings → Security, which explains it and asks for a second click.
  const canSignOut = () => has('auth.logout');
  command({
    id: 'auth.logout', title: 'Sign Out of This Browser', category: 'Security', icon: 'lock', when: canSignOut,
    run: () => api.logout(false).then(() => location.reload(), (e) => toast({ kind: 'error', title: 'Could not sign out', message: e.message })),
  });
  command({ id: 'auth.logoutAll', title: 'Sign Out of All Browsers…', category: 'Security', icon: 'lock', when: canSignOut, run: () => load.settings().then((m) => m.openSettings({ section: 'security' })) });

  // Tabs
  command({ id: 'tab.close', title: 'Close Tab', category: 'Tabs', icon: 'x', keys: ['Alt+W'], desktopKeys: ['Mod+W'], when: hasFile, run: () => editor.close() });
  command({ id: 'tab.reopen', title: 'Reopen Closed Tab', category: 'Tabs', icon: 'history', keys: ['Alt+Shift+T'], desktopKeys: ['Mod+Shift+T'], run: () => editor.reopenClosed() });
  command({ id: 'tab.next', title: 'Next Tab', category: 'Tabs', icon: 'chevron-right', keys: ['Alt+]'], desktopKeys: ['Ctrl+Tab'], run: () => editor.next() });
  command({ id: 'tab.prev', title: 'Previous Tab', category: 'Tabs', icon: 'chevron-left', keys: ['Alt+['], desktopKeys: ['Ctrl+Shift+Tab'], run: () => editor.prev() });
  command({ id: 'tab.pin', title: 'Keep Tab Open', category: 'Tabs', icon: 'check', when: hasFile, run: () => editor.pin(editor.active) });
  command({ id: 'md.toggle', title: 'Toggle Markdown Preview', category: 'View', icon: 'eye', keys: ['Alt+M'], when: () => codeView()?.kind === 'markdown', run: () => codeView().toggleMode() });
  for (let i = 1; i <= 9; i++) command({ id: `tab.${i}`, title: `Open Tab ${i}`, category: 'Tabs', hidden: true, keys: [`Alt+${i}`], run: () => editor.selectTab(i - 1) });

  // File
  command({ id: 'file.reveal', title: 'Reveal Active File in Explorer', category: 'File', icon: 'eye', when: hasFile, run: () => { shell.showPanel('files', { focus: false }); bus.emit('tree:reveal', { path: editor.active }); } });
  command({
    id: 'copy.ref', title: 'Copy Reference (@path:line)', category: 'File', icon: 'copy', keys: ['Alt+C'], when: hasFile,
    run: async () => {
      const sel = codeView()?.selection?.();
      const ref = sel ? `@${sel.path}:${sel.start === sel.end ? sel.start : `${sel.start}-${sel.end}`}` : `@${editor.active}`;
      await navigator.clipboard?.writeText(ref).catch(() => {});
      toast({ kind: 'ok', title: 'Copied reference', message: ref, timeout: 2000 });
    },
  });
  command({
    id: 'copy.path', title: 'Copy Path', category: 'File', icon: 'copy', when: hasFile,
    run: async () => {
      await navigator.clipboard?.writeText(editor.active).catch(() => {});
      toast({ kind: 'ok', title: 'Copied path', message: editor.active, timeout: 2000 });
    },
  });

  // Workspace
  command({
    id: 'index.rebuild', title: 'Rebuild File Index', category: 'Workspace', icon: 'refresh', keys: ['Mod+Shift+R'],
    run: async () => {
      try {
        await api.rebuildIndex();
        getTree()?.refresh();
        toast({ title: 'Rebuilding index…', timeout: 1500 });
      } catch (e) {
        toast({ kind: 'error', title: 'Could not rebuild the index', message: e.message });
      }
    },
  });
  command({ id: 'pr.open', title: 'Open Pull Request…', category: 'Review', icon: 'git-pull-request', run: (url) => openPullRequest(url) });
}

// PR review opens through the server's pr.open job (fetch + worktree can take a while on a big
// repo). One toast follows its steps; the `workspace` event then moves the whole UI onto the PR.
const PR_STEPS = { metadata: 'Reading the pull request…', fetch: 'Fetching commits…', worktree: 'Checking out…', 'merge-base': 'Finding the merge base…' };
const PR_OPEN_LIMIT_MS = 40 * 60 * 1000; // server-side clone timeout is 30 min

async function openPullRequest(url) {
  if (!has('pr.open') || !url) {
    toast(has('pr.open')
      ? { title: 'Paste a pull request URL into the command palette', message: 'ferro opens it for review in place.' }
      : { title: 'In-app PR review is coming soon', message: 'For now, start ferro with the PR URL: ferro <pr-url>' });
    return;
  }
  const t = toast({ title: 'Opening pull request…', message: url, timeout: 0 });
  try {
    const { job } = await api.openPr(url);
    const deadline = Date.now() + PR_OPEN_LIMIT_MS;
    while (Date.now() < deadline) {
      await new Promise((r) => setTimeout(r, 400));
      const j = await api.job(job.id);
      if (j.state === 'done') {
        const pr = j.result || {};
        t.close();
        toast({ kind: 'ok', title: `Reviewing ${pr.owner}/${pr.repo}#${pr.number}`, message: pr.title });
        return;
      }
      if (j.state === 'failed' || j.state === 'cancelled') throw new Error(j.error?.message || `The job was ${j.state}.`);
      const step = j.progress?.message;
      if (step) t.update({ message: PR_STEPS[step] || step });
    }
    throw new Error('Timed out waiting for the checkout.');
  } catch (e) {
    t.close();
    toast({ kind: 'error', title: 'Could not open the pull request', message: e.message });
  }
}

boot().catch((e) => {
  console.error(e);
  const root = document.getElementById('app');
  root.removeAttribute('aria-busy');
  mount(root, h('div', { class: 'auth-screen' }, h('div', { class: 'auth-card' }, icon('alert', 'xl'), h('h1', null, 'ferro failed to start'), h('pre', { class: 'auth-code' }, String(e?.stack || e)))));
});
