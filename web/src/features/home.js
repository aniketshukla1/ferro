// Home view: workspace header, search entry point, recent files, working-tree changes, key hints.
import { h, mount } from '../core/dom.js';
import { keysEl } from '../core/keys.js';
import { execute } from '../core/commands.js';
import { store } from '../core/store.js';
import { basename, dirname, formatBytes, formatCount, formatMs, plural } from '../core/util.js';
import { icon, fileIcon, folderIcon } from '../ui/icons.js';
import { session } from './session.js';
import { revealDir } from './panels.js';

const HINTS = [
  ['Mod+K', 'Go to file'],
  ['Mod+Shift+F', 'Search in files'],
  ['Mod+Shift+P', 'Commands'],
  ['Mod+G', 'Go to line'],
  ['?', 'All shortcuts'],
];
const MAX_ROWS = 8;

export function createHome({ onOpen }) {
  const title = h('h1', { class: 'home-title' }, 'ferro');
  const meta = h('p', { class: 'home-meta' });
  const recentList = h('div', { class: 'home-list' });
  const changesList = h('div', { class: 'home-list' });
  const changesHead = h('div', { class: 'home-col-head' }, h('h2', { class: 'label' }, 'Changes'));
  const changesCol = h('section', { class: 'home-col', 'aria-label': 'Changes' }, changesHead, changesList);
  const foot = h('p', { class: 'home-foot' });

  const el = h('div', { class: 'home-inner' },
    h('header', { class: 'home-head' }, title, meta),
    h('button', { class: 'home-search', on: { click: () => execute('palette.files') } },
      icon('search'), h('span', { class: 'hs-text' }, 'Search files, symbols, text'), keysEl('Mod+K')),
    h('div', { class: 'home-cols' },
      h('section', { class: 'home-col', 'aria-label': 'Recent files' }, h('div', { class: 'home-col-head' }, h('h2', { class: 'label' }, 'Recent')), recentList),
      changesCol),
    h('ul', { class: 'home-hints', 'aria-label': 'Shortcuts' }, HINTS.map(([k, t]) => h('li', null, keysEl(k), h('span', null, t)))),
    foot);

  function row(path, { code, dir, onClick }) {
    return h('button', { class: 'home-row', title: path, on: { click: onClick } },
      dir ? folderIcon(false) : fileIcon(path),
      h('span', { class: 'hr-name' }, basename(path)),
      h('span', { class: 'hr-dir' }, dirname(path)),
      code ? h('span', { class: `gitc ${code === '?' ? 'A' : code}` }, code === '?' ? 'U' : code) : null);
  }

  function renderMeta() {
    const m = store.get('meta');
    const idx = store.get('index');
    const git = store.get('git');
    title.textContent = m?.workspace?.name || 'ferro';
    const parts = [];
    const branch = git?.branch || m?.workspace?.branch;
    if (branch) parts.push(h('span', { class: 'hm-branch' }, icon('git-branch', 'xs'), branch));
    if (git && (git.ahead || git.behind)) parts.push(h('span', null, `↑${git.ahead} ↓${git.behind}`));
    if (idx) {
      parts.push(idx.state === 'ready'
        ? h('span', null, `${formatCount(idx.files)} files · indexed in ${formatMs(idx.ms)}`)
        : h('span', { class: 'row' }, h('span', { class: 'spinner' }), 'Indexing…'));
    }
    if (m?.host === 'desktop') parts.push(h('span', null, 'Desktop'));
    mount(meta, parts.flatMap((p, i) => (i ? [h('span', { class: 'hm-dot', 'aria-hidden': 'true' }, '·'), p] : [p])));
  }

  function renderRecent() {
    const recent = session.data.recent.slice(0, MAX_ROWS);
    if (!recent.length) {
      mount(recentList, h('p', { class: 'home-empty' }, 'Files you open show up here.'));
      return;
    }
    mount(recentList, recent.map((p) => row(p, { onClick: () => onOpen(p, { preview: false, focus: true }) })));
  }

  function renderChanges() {
    const git = store.get('git');
    changesCol.hidden = !git && !store.get('meta')?.workspace?.git;
    if (!git) { mount(changesList, h('p', { class: 'home-empty' }, 'Loading…')); return; }
    const files = git.files;
    mount(changesHead, h('h2', { class: 'label' }, 'Changes'), files.length ? h('span', { class: 'count' }, formatCount(files.length)) : null);
    if (!files.length) {
      mount(changesList, h('p', { class: 'home-empty' }, `Working tree clean on ${git.branch || 'this branch'}.`));
      return;
    }
    const rows = files.slice(0, MAX_ROWS).map((f) => row(f.path, {
      dir: f.dir,
      code: f.conflicted ? 'U' : f.untracked ? '?' : f.worktree || f.index,
      onClick: () => (f.dir ? revealDir(f.path) : onOpen(f.path, { preview: true, focus: false })),
    }));
    if (files.length > MAX_ROWS) {
      rows.push(h('button', { class: 'home-more', on: { click: () => execute('panel.changes') } },
        `All ${plural(files.length, 'change')}`, icon('chevron-right', 'xs')));
    }
    mount(changesList, rows);
  }

  function renderFoot() {
    const m = store.get('meta');
    const met = store.get('metrics');
    const bits = [];
    if (m?.version) bits.push(`ferro ${m.version}`);
    if (met?.rssBytes) bits.push(`server ${formatBytes(met.rssBytes)}`);
    foot.textContent = bits.join(' · ');
  }

  function refresh() {
    renderMeta();
    renderRecent();
    renderChanges();
    renderFoot();
  }

  for (const k of ['meta', 'index', 'git']) store.subscribe(k, () => { renderMeta(); renderChanges(); });
  store.subscribe('metrics', renderFoot);
  refresh();

  return { el, refresh };
}
