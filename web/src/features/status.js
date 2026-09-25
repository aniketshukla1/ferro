// Status bar: branch, changes, index state | cursor, language, encoding, memory, latency, connection.
import { h, mount } from '../core/dom.js';
import { execute } from '../core/commands.js';
import { store } from '../core/store.js';
import { formatBytes, formatCount, formatMs, rafThrottle } from '../core/util.js';
import { icon } from '../ui/icons.js';

export function createStatusBar(el, { editor, mockMode }) {
  const left = h('div', { class: 'sb-group' });
  const right = h('div', { class: 'sb-group right' });
  mount(el, left, right);

  const item = (content, opts = {}) => (opts.onClick
    ? h('button', { class: 'sb-item', 'data-tip': opts.tip, 'data-tip-side': 'top', on: { click: opts.onClick } }, content)
    : h('span', { class: 'sb-item', 'data-tip': opts.tip, 'data-tip-side': 'top' }, content));

  const render = rafThrottle(() => {
    const git = store.get('git');
    const idx = store.get('index');
    const cur = store.get('cursor');
    const met = store.get('metrics');
    const timing = store.get('timing');
    const conn = store.get('conn');
    const meta = editor.activeMeta();

    const L = [];
    if (git) {
      const sync = [];
      if (git.ahead) sync.push(h('span', null, `↑${git.ahead}`));
      if (git.behind) sync.push(h('span', null, `↓${git.behind}`));
      L.push(item([icon('git-branch'), h('span', null, git.branch || 'detached'), ...sync], { tip: git.upstream ? `Tracking ${git.upstream}` : 'Current branch', onClick: () => execute('panel.changes') }));
      const n = git.counts.staged + git.counts.unstaged + git.counts.untracked;
      L.push(item([icon('circle-dot'), h('span', null, n ? `${formatCount(n)} changed` : 'clean')], { tip: 'Working tree changes', onClick: () => execute('panel.changes') }));
    }
    if (idx) {
      L.push(idx.state === 'ready'
        ? item([icon('zap'), h('span', { class: 'num' }, `${formatCount(idx.files)} files · ${formatMs(idx.ms)}`)], { tip: 'Rebuild index', onClick: () => execute('index.rebuild') })
        : item([h('span', { class: 'spinner' }), h('span', null, 'Indexing…')]));
    }

    const R = [];
    if (cur && !cur.image) {
      const text = cur.preview
        ? `Preview · ${formatCount(cur.total)} lines`
        : cur.selEnd > cur.selStart
          ? `Ln ${formatCount(cur.selStart)}–${formatCount(cur.selEnd)} (${formatCount(cur.selEnd - cur.selStart + 1)} lines)`
          : `Ln ${formatCount(cur.line)} of ${formatCount(cur.total)}`;
      R.push(cur.preview
        ? item(h('span', { class: 'num' }, text), { tip: 'Show source (Alt+M)', onClick: () => execute('md.toggle') })
        : item(h('span', { class: 'num' }, text), { tip: 'Go to line', onClick: () => execute('palette.line') }));
    }
    if (cur?.language) R.push(item(h('span', null, cur.language)));
    if (meta && meta.kind === 'text') R.push(item(h('span', null, `${meta.encoding === 'utf-8-lossy' ? 'UTF-8 (lossy)' : 'UTF-8'} · ${(meta.eol || 'lf').toUpperCase()}`)));
    if (met) R.push(item([icon('gauge'), h('span', { class: 'num' }, formatBytes(met.rssBytes))], { tip: `ferro server memory · CPU ${met.cpuPct}%` }));
    if (timing) {
      const ms = timing.serverMs ?? timing.clientMs;
      R.push(item([icon('activity'), h('span', { class: 'num' }, `${timing.label} ${formatMs(ms)}`)], { tip: `Last ${timing.label}: ${formatMs(timing.serverMs)} server · ${formatMs(timing.clientMs)} round trip` }));
    }
    const dotCls = conn === 'connected' ? 'conn-dot' : conn === 'connecting' ? 'conn-dot wait' : 'conn-dot bad';
    const label = mockMode ? 'mock' : conn === 'connected' ? 'live' : conn;
    R.push(item([h('span', { class: dotCls }), h('span', null, label)], { tip: mockMode ? 'Mock mode: no backend, fixture data' : `Server ${conn}` }));

    mount(left, L);
    mount(right, R);
  });

  for (const k of ['git', 'index', 'cursor', 'metrics', 'timing', 'conn', 'active']) store.subscribe(k, render);
  render();
  return { render };
}
