// ferro frontend entry: boot, feature wiring, commands and shortcuts.
// Boot graph: shell, tree, tabs, code viewer, home, status bar. Everything else loads on first
// use and is warmed up while the browser is idle (FRONTEND.md § 3.1).
import { h, mount } from './core/dom.js';
import { api, has, request } from './core/api.js';
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
  markdown: lazy(() => import('./features/markdown.js')),
  image: lazy(() => import('./features/image.js')),
  diff: lazy(() => import('./features/diff.js')),
  review: lazy(() => import('./features/review.js')),
  ai: lazy(() => import('./features/ai.js')),
  nav: lazy(() => import('./features/nav.js')),
  agent: lazy(() => import('./features/agent.js')),
  inlineEdit: lazy(() => import('./features/inline-edit.js')),
  threads: lazy(() => import('./features/threads.js')),
  problems: lazy(() => import('./features/problems.js')),
  update: lazy(() => import('./features/update.js')),
  history: lazy(() => import('./features/history.js')),
  explain: lazy(() => import('./features/explain.js')),
  checks: lazy(() => import('./features/checks.js')),
  memory: lazy(() => import('./features/memory.js')),
  hud: lazy(() => import('./features/hud.js')),
  vim: lazy(() => import('./features/vim.js')),
};

let hudToggle = () => {};

async function boot() {
  performance.mark('ferro:boot');
  const root = document.getElementById('app');
  const params = new URLSearchParams(location.search);
  let mockMode = params.has('mock') ? (params.get('mock') || '1') : null;
  // Release builds do not ship the mock backend: ?mock=1 then just uses the real server.
  if (mockMode) await import('./mock/index.js').then((m) => m.installMock(mockMode), () => { mockMode = null; });
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
      // One button: Collapse all while any folder is open, Expand all once none is.
      const foldBtn = h('button', { class: 'icon-btn sm tree-fold-btn', on: { click: () => (tree?.anyOpen ? tree.collapseAll() : tree?.expandAll()) } });
      const setFold = (open) => {
        const label = open ? 'Collapse all' : 'Expand all';
        foldBtn.setAttribute('aria-label', label);
        foldBtn.dataset.tip = label;
        mount(foldBtn, icon(open ? 'collapse-all' : 'expand-all', 'sm'));
      };
      setFold(false);
      mount(section,
        h('div', { class: 'panel-head' },
          h('span', { class: 'panel-title truncate' }, meta.workspace?.name || 'Files'),
          h('div', { class: 'panel-actions' },
            h('button', { class: 'icon-btn sm', 'aria-label': 'Reveal active file', 'data-tip': 'Reveal active file', on: { click: () => execute('file.reveal') } }, icon('eye', 'sm')),
            foldBtn,
            h('button', { class: 'icon-btn sm', 'aria-label': 'Toggle ignored files', 'aria-pressed': 'false', 'data-tip': 'Show ignored files', on: { click: (e) => { const on = tree?.toggleIgnored(); const b = e.currentTarget; b.classList.toggle('active', on); b.setAttribute('aria-pressed', String(on)); b.dataset.tip = on ? 'Hide ignored files' : 'Show ignored files'; } } }, icon('eye-off', 'sm')),
            h('button', { class: 'icon-btn sm', 'aria-label': 'Refresh', 'data-tip': 'Refresh', on: { click: () => execute('index.rebuild') } }, icon('refresh', 'sm')))),
        body);
      tree = createTree(body, { onOpen, onOpenState: setFold });
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
  // History: commits on any ref, commit view, compare, branch switch.
  let historyPanel = null;
  const historyCtx = { getDiffView: () => getDiffView(), onOpen, closeSidebar: () => { if (narrow.matches && session.data.layout.sidebar) shell.toggleSidebar(); } };
  if (has('git.history')) shell.registerPanel({ id: 'history', title: 'History', icon: 'history', keys: ['Mod+Shift+H'], render: async (s) => { historyPanel = (await load.history()).renderHistoryPanel(s, historyCtx); }, onShow: ({ focus }) => { if (focus) historyPanel?.focus(); } });
  store.subscribe('git', (g) => changesPanel.setBadge(g ? g.counts.staged + g.counts.unstaged + g.counts.untracked : 0));

  // Palette and find are created on first use; these proxies keep call sites synchronous-looking.
  const getPalette = lazy(async () => (await load.palette()).createPalette({ editor, onOpen }));
  const palette = { open: (...a) => getPalette().then((p) => p.open(...a)) };
  const getFind = lazy(async () => (await load.find()).createFind({ editor, host: shell.viewsEl }));
  const find = { open: () => getFind().then((f) => f.open()), next: () => getFind().then((f) => f.next()), prev: () => getFind().then((f) => f.prev()) };
  let diffView = null; // the Ask panel peeks at it for context without forcing the lazy load
  const getDiffView = lazy(async () => (diffView = (await load.diff()).createDiffView(shell.viewsEl, { onOpen })));
  bus.on('diff:open', (opts) => getDiffView().then((d) => d.show(opts)));
  // AI change notes on the open diff.
  bus.on('ai:explain', async () => {
    const d = await getDiffView();
    if (d.el.hidden) await d.show({});
    (await load.explain()).explainOpenDiff(d);
  });
  bus.on('diff:toggle', () => getDiffView().then((d) => {
    if (d.el.hidden) d.show({ path: store.get('active') });
    else d.hide();
  }));

  // ---------- inspector (tabs render the first time the inspector is shown) ----------
  const aiTab = shell.registerInspectorTab({ id: 'ai', title: 'AI', icon: 'sparkles', render: async (el) => (await load.ai()).renderAiTab(el, { editor, onOpen, getDiffView, peekDiffView: () => diffView }) });
  shell.registerInspectorTab({ id: 'info', title: 'Info', icon: 'info', render: async (el) => (await load.chrome()).renderInfoTab(el) });

  // Code navigation (F5): the viewer reports positions; nav.js loads on first use.
  const navCtx = {
    onOpen,
    pick: (title, items) => palette.open('', { special: 'pick', title, items }),
    openRefs: () => { shell.toggleInspector(true); refsTab?.show(); },
  };
  // Agent threads (API.md § 10.7): saved conversations with the coding agent.
  const threadsTab = has('harness.threads') ? shell.registerInspectorTab({ id: 'threads', title: 'Agent', icon: 'terminal', render: async (el) => (await load.threads()).renderThreadsTab(el, { editor, onOpen, getDiffView }) }) : null;
  bus.on('threads:open', () => { shell.toggleInspector(true); threadsTab?.show(); });
  // Problems (API.md § 4.9): language-server diagnostics; problems.js keeps them in sync.
  const problemsTab = has('lsp.diagnostics') ? shell.registerInspectorTab({ id: 'problems', title: 'Problems', icon: 'alert', render: async (el) => (await load.problems()).renderProblemsTab(el, { onOpen }) }) : null;
  if (problemsTab) load.problems().then((m) => m.installProblems({ editor }));
  bus.on('problems:show', () => { shell.toggleInspector(true); problemsTab?.show(); });
  // Checks (API.md § 16): does the open diff break anything?
  const checksTab = has('checks.breaking') ? shell.registerInspectorTab({ id: 'checks', title: 'Checks', icon: 'check-circle', render: async (el) => (await load.checks()).renderChecksTab(el, { getDiffView, peekDiffView: () => diffView, onOpen }) }) : null;
  bus.on('checks:open', () => { shell.toggleInspector(true); checksTab?.show(); });
  // Team review memory (API.md § 17): rules from what reviewers dismiss and accept.
  const memoryTab = has('memory') ? shell.registerInspectorTab({ id: 'memory', title: 'Memory', icon: 'layers', render: async (el) => (await load.memory()).renderMemoryTab(el) }) : null;
  bus.on('memory:open', () => { shell.toggleInspector(true); memoryTab?.show(); });
  const refsTab = has('nav') ? shell.registerInspectorTab({ id: 'refs', title: 'References', icon: 'list-tree', render: async (el) => (await load.nav()).renderRefsTab(el, navCtx) }) : null;
  const nav = (fn, pos) => pos && load.nav().then((m) => m[fn](pos, navCtx));
  bus.on('nav:definition', (pos) => nav('goToDefinition', pos));
  bus.on('nav:hover', (pos) => nav('showHover', pos));

  createStatusBar(shell.statusEl, { editor, mockMode });
  watchConnection(shell.banners);
  bus.on('toast', (t) => toast(t));

  registerCommands({ shell, editor, palette, find, getTree: () => tree, getSearch: () => search, aiTab, nav, getDiffView, historyCtx });

  // Narrow screens start with the sidebar closed (in memory only, not saved).
  if (narrow.matches) session.data.layout.sidebar = false;
  shell.applyLayout();
  editor.restore(session.data);
  performance.mark('ferro:ready');

  // Latency HUD (§ 9): ui.hud, or Mod+Alt+P (which also saves the choice).
  let hudOn = false;
  const setHud = (on, persist) => {
    if (on === hudOn) return;
    hudOn = on;
    load.hud().then((m) => m.toggleHud(on, { onClose: () => setHud(false, true) }));
    if (persist) api.putSettings({ 'ui.hud': on }).catch(() => {});
  };
  hudToggle = () => setHud(!hudOn, true);
  setHud(store.get('settings')?.['ui.hud'] === true, false);
  // Vim keys in the code viewer (§ 6.18): ui.keymap = "vim".
  let vimOn = false;
  const setVim = (on) => {
    if (on === vimOn) return;
    vimOn = on;
    load.vim().then((m) => (on ? m.enableVim({ editor, statusEl: shell.statusEl }) : m.disableVim()));
  };
  setVim(store.get('settings')?.['ui.keymap'] === 'vim');

  // Settings changed here or in another window: apply presentation keys live.
  let codeFs = store.get('settings')?.['ui.codeFontSize'];
  store.subscribe('settings', (v) => {
    applySettingsTheme(v);
    applyUiSettings(v);
    setHud(v?.['ui.hud'] === true, false);
    setVim(v?.['ui.keymap'] === 'vim');
    if (v?.['ui.codeFontSize'] !== codeFs) { codeFs = v?.['ui.codeFontSize']; editor.resetViews(); }
  });

  // Keep the tree on the active file (only scrolls when the row is off-screen). ui.autoReveal: false opts out.
  store.subscribe('active', (p) => {
    if (p && autoReveal()) bus.emit('tree:reveal', { path: p, align: 'auto' });
  });

  // Links (`ferro open`, editors): ?path=&line=&view=, over the file ferro started with.
  const initial = params.get('path') ? { path: params.get('path'), line: Number(params.get('line')) || undefined } : meta.initial;
  const view = { changes: 'panel.changes', checks: 'checks.open', history: 'git.history', diff: 'diff.toggle' }[params.get('view')];
  const opened = initial?.path ? editor.open(initial.path, { line: initial.line, focus: true }) : null;
  if (view) Promise.resolve(opened).then(() => execute(view));
  const clean = new URL(location.href);
  ['path', 'line', 'view'].forEach((k) => clean.searchParams.delete(k));
  history.replaceState(null, '', clean);

  connectEvents({ metrics: true });
  // Git status.
  const refreshGit = () => (has('git.status.v2') ? api.gitStatus().then((g) => { if (g) store.set('git', g); }).catch(() => {}) : Promise.resolve(null));
  if (has('git.status.v2')) refreshGit();
  bus.on('git:refresh', refreshGit);
  // A switched workspace (PR, folder): fresh meta and status; the tree and editor reset themselves.
  bus.on('ev:workspace', () => {
    api.meta().then((m) => store.set('meta', m)).catch(() => {});
    refreshGit();
  });

  // Review mode: the PR bar and conversation tab (review.js).
  if (meta.pr) store.set('pr', meta.pr);
  store.subscribe('pr', (pr) => { if (pr) load.review().then((m) => m.installReview(shell)); }, { now: true });

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

function registerCommands({ shell, editor, palette, find, getTree, getSearch, aiTab, nav, getDiffView, historyCtx }) {
  const hasFile = () => !!editor.active;
  const codeView = () => editor.activeView();
  // Find works on source text; in the Markdown preview the browser's own find stays available.
  const findable = () => { const v = codeView(); return v?.kind === 'code' || (!!v?.sourceView && v.mode === 'source'); };
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
  const navOk = () => has('nav') && codeView()?.kind === 'code';
  command({ id: 'nav.definition', title: 'Go to Definition', category: 'Go', icon: 'enter', keys: ['F12'], when: navOk, run: () => nav('goToDefinition', codeView().position()) });
  command({ id: 'nav.references', title: 'Find All References', category: 'Go', icon: 'list-tree', keys: ['Shift+F12'], when: navOk, run: () => nav('findReferences', codeView().position()) });
  command({ id: 'palette.wsymbols', title: 'Go to Symbol in Workspace…', category: 'Go', icon: 'hash', desktopKeys: ['Mod+T'], run: () => palette.open('#') });
  command({
    id: 'search.usages', title: 'Find Usages of Selection', category: 'Go', icon: 'search', keys: ['Alt+U'], when: hasFile,
    run: async () => {
      // The selection, else the name under the caret; searched as a whole word across files.
      const q = window.getSelection()?.toString().trim().split('\n')[0] || codeView()?.position?.().word || '';
      await shell.showPanel('search');
      getSearch()?.focus(q, { word: true });
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
  command({ id: 'diff.toggle', title: 'Toggle Diff View', category: 'View', icon: 'git-compare', keys: ['Mod+D'], run: () => bus.emit('diff:toggle') });
  command({ id: 'panel.outline', title: 'Show Outline', category: 'View', icon: 'list-tree', run: () => shell.showPanel('outline') });
  command({ id: 'hud.toggle', title: 'Toggle Latency HUD', category: 'View', icon: 'activity', keys: ['Mod+Alt+P'], run: () => hudToggle() });
  command({ id: 'theme.pick', title: 'Color Theme…', category: 'Preferences', icon: 'contrast', run: () => palette.open('', { special: 'theme' }) });
  command({ id: 'theme.cycle', title: 'Next Color Theme', category: 'Preferences', icon: 'contrast', run: () => { const t = cycleTheme(); toast({ title: `Theme: ${t.name}`, timeout: 1500 }); } });
  command({ id: 'theme.toggle', title: 'Toggle Light / Dark', category: 'Preferences', icon: 'contrast', keys: ['Alt+Shift+L'], run: () => toggleLightDark() });
  command({ id: 'settings.open', title: 'Open Settings', category: 'Preferences', icon: 'settings', keys: ['Mod+,'], run: () => load.settings().then((m) => m.openSettings()) });
  const help = () => load.chrome().then((m) => m.showKeyboardHelp());
  command({ id: 'help.keys', title: 'Keyboard Shortcuts', category: 'Help', icon: 'keyboard', keys: ['Mod+/'], run: help });
  bindKey('?', help);
  command({ id: 'agent.edit', title: 'Edit with Agent…', category: 'AI', icon: 'terminal', keys: ['Alt+E'], when: () => has('harness') && codeView()?.kind === 'code', run: () => load.agent().then((m) => m.editWithAgent({ editor, getDiffView })) });
  // Inline edit (§ 6.25)
  const editable = () => has('file.edit') && codeView()?.kind === 'code' && !store.get('meta')?.readOnly;
  const inline = (ai) => load.inlineEdit().then((m) => m.inlineEdit(editor, ai));
  command({ id: 'edit.inline', title: 'Edit Lines', category: 'File', icon: 'pencil', keys: ['Alt+I'], when: editable, run: () => inline(false) });
  command({ id: 'edit.ai', title: 'Edit Lines with AI…', category: 'AI', icon: 'sparkles', keys: ['Alt+K'], when: () => editable() && has('ai.edit'), run: () => inline(true) });
  bus.on('view:select', () => { if (editable()) load.inlineEdit().then((m) => m.offerBar(editor)); });
  // Background updates (API.md § 13): state in store 'update'; update.js acts on it.
  if (has('update.auto')) {
    bus.on('ev:update', (st) => store.set('update', { ...store.get('update'), ...st }));
    whenIdle(() => request('update').then((st) => store.set('update', st)).catch(() => {}));
  }
  command({ id: 'update.check', title: 'Check for Updates', category: 'Help', icon: 'refresh', when: () => has('update.auto'), run: async () => (await load.update()).checkNow() });
  command({ id: 'update.install', title: 'Install Update', category: 'Help', icon: 'arrow-down', when: () => store.get('update')?.state === 'available' && store.get('update')?.canInstall, run: async () => (await load.update()).install() });
  command({ id: 'update.restart', title: 'Restart to Update', category: 'Help', icon: 'refresh', when: () => store.get('update')?.state === 'ready', run: async () => (await load.update()).restartNow() });
  command({ id: 'ai.explain', title: 'Explain Changes', category: 'AI', icon: 'sparkles', when: () => has('ai.explain'), run: () => bus.emit('ai:explain') });
  command({ id: 'memory.open', title: 'Team Review Memory', category: 'AI', icon: 'layers', when: () => has('memory'), run: () => bus.emit('memory:open') });
  command({ id: 'memory.convention', title: 'Add Team Convention…', category: 'AI', icon: 'plus', when: () => has('memory'), run: async () => (await load.memory()).openRuleDialog({ kind: 'convention', appliesTo: 'ai' }) });
  command({ id: 'checks.open', title: 'Check This Change', category: 'Git', icon: 'check-circle', when: () => has('checks.breaking'), run: () => bus.emit('checks:open') });
  command({ id: 'git.history', title: 'Show History', category: 'Git', icon: 'history', keys: ['Mod+Shift+H'], when: () => has('git.history'), run: () => shell.showPanel('history') });
  command({ id: 'git.compare', title: 'Compare Revisions…', category: 'Git', icon: 'git-compare', when: () => has('git.history'), run: async () => (await load.history()).openCompareDialog(historyCtx) });
  command({ id: 'git.checkout', title: 'Switch Branch…', category: 'Git', icon: 'git-branch', when: () => has('git.history') && !store.get('pr'), run: async () => (await load.history()).openBranchPicker() });
  command({ id: 'problems.show', title: 'Show Problems', category: 'View', icon: 'alert', when: () => has('lsp.diagnostics'), run: () => bus.emit('problems:show') });
  command({ id: 'threads.open', title: 'Agent Threads', category: 'AI', icon: 'terminal', when: () => has('harness.threads'), run: () => bus.emit('threads:open') });
  command({ id: 'ai.review', title: 'AI Review…', category: 'AI', icon: 'sparkles', when: () => has('ai.review'), run: () => { store.set('aiMode', 'review'); shell.toggleInspector(true); aiTab.show(); } });
  command({ id: 'ai.ask', title: 'Ask AI', category: 'AI', icon: 'sparkles', keys: ['Mod+I'], run: () => { store.set('aiMode', 'ask'); shell.toggleInspector(true); aiTab.show(); } });

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
  command({ id: 'md.toggle', title: 'Toggle Preview (Markdown, CSV)', category: 'View', icon: 'eye', keys: ['Alt+M'], when: () => !!codeView()?.toggleMode, run: () => codeView().toggleMode() });
  command({
    id: 'view.toggleWrap', title: 'Toggle Word Wrap', category: 'View', icon: 'wrap', keys: ['Alt+Z'],
    when: findable,
    run: () => { const v = codeView(); (v?.sourceView ? v.sourceView() : v)?.toggleWrap?.(); },
  });
  for (let i = 1; i <= 9; i++) command({ id: `tab.${i}`, title: `Open Tab ${i}`, category: 'Tabs', hidden: true, keys: [`Alt+${i}`], run: () => editor.selectTab(i - 1) });

  // File
  command({ id: 'file.reveal', title: 'Reveal Active File in Explorer', category: 'File', icon: 'eye', when: hasFile, run: () => { shell.showPanel('files', { focus: false }); bus.emit('tree:reveal', { path: editor.active }); } });
  const chrome = (fn, ...a) => load.chrome().then((m) => m[fn](...a));
  command({ id: 'copy.ref', title: 'Copy Reference (@path:line)', category: 'File', icon: 'copy', keys: ['Alt+C'], when: hasFile, run: () => chrome('copyRef', editor.active, codeView()?.selection?.()) });
  command({ id: 'copy.path', title: 'Copy Path', category: 'File', icon: 'copy', when: hasFile, run: () => chrome('copyPath', editor.active) });

  // Workspace
  command({ id: 'index.rebuild', title: 'Rebuild File Index', category: 'Workspace', icon: 'refresh', keys: ['Mod+Shift+R'], run: () => chrome('rebuildIndex', getTree()) });
  command({ id: 'pr.open', title: 'Open Pull Request…', category: 'Review', icon: 'git-pull-request', run: (url) => load.chrome().then((m) => m.openPullRequest(url)) });
}

boot().catch((e) => {
  console.error(e);
  const root = document.getElementById('app');
  root.removeAttribute('aria-busy');
  mount(root, h('div', { class: 'auth-screen' }, h('div', { class: 'auth-card' }, icon('alert', 'xl'), h('h1', null, 'ferro failed to start'), h('pre', { class: 'auth-code' }, String(e?.stack || e)))));
});
