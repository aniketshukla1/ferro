// History (API.md § 6.6–6.10, FRONTEND.md § 6.21): every commit on any branch, tag or all refs,
// searchable; a commit opens in the diff view against its parent; any two revisions compare
// (Cmd/Ctrl-click a second commit, or the Compare dialog); branches switch only on an explicit
// action, and never over local changes. Loads on first use.
import { h, mount } from '../core/dom.js';
import { request } from '../core/api.js';
import { store } from '../core/store.js';
import { bus } from '../core/bus.js';
import { debounce, formatCount, isMac, plural, relTime } from '../core/util.js';
import { VirtualList } from '../core/virtual.js';
import { icon } from '../ui/icons.js';
import { toast } from '../ui/overlay.js';
import { openDialog } from '../ui/dialog.js';

const PAGE = 100;
/** Ref chips for a commit: no HEAD, and no `remote/x` when `x` itself is listed. */
const shownRefs = (refs) => refs.filter((r) => r !== 'HEAD' && !refs.some((o) => o !== r && r === `${r.split('/')[0]}/${o}`));
const ROW_H = 46;
const isSha = (s) => /^[0-9a-f]{7,64}$/i.test(s || '');
const short = (rev) => (isSha(rev) ? rev.slice(0, 7) : rev);

let refsCache = null;
async function loadRefs(force = false) {
  if (!refsCache || force) refsCache = request('git/refs').catch((e) => { refsCache = null; throw e; });
  return refsCache;
}

// ---------- commit view ----------

let endCommitView = null;

// The commit header can fold to its summary line; the choice sticks across commits and reloads.
const FOLD_KEY = 'ferro.commitHeader';
const foldedPref = () => { try { return localStorage.getItem(FOLD_KEY) === 'min'; } catch { return false; } };

/** Open one commit in the diff view: its changes against its first parent, with a header. */
export async function showCommit(rev, ctx) {
  let d;
  try {
    d = await request('git/show', { query: { rev } });
  } catch (e) {
    toast({ kind: 'error', title: 'Could not open that commit', message: e.message });
    return;
  }
  const dv = await ctx.getDiffView();
  endCommitView?.();
  const offs = [];
  const end = () => { endCommitView = null; for (const off of offs) off(); dv.setBanner(null); };
  endCommitView = end;
  const bodyEl = d.body ? h('pre', { class: 'hc-body' }, d.body) : null;
  const more = bodyEl && (d.body.split('\n').length > 3 || d.body.length > 240)
    ? h('button', { class: 'link-btn small hc-more', on: { click: () => { const open = bodyEl.classList.toggle('open'); more.textContent = open ? 'Less' : 'More'; } } }, 'More')
    : null;
  const fold = h('button', { class: 'icon-btn sm hc-fold', type: 'button', on: { click: () => setFolded(!bar.classList.contains('hc-min'), true) } });
  const bar = h('div', { class: 'hc-bar', role: 'region', 'aria-label': 'Commit' },
    h('div', { class: 'hc-top' },
      icon('git-commit', 'sm'),
      h('span', { class: 'hc-subject', title: d.subject }, d.subject),
      d.refs.filter((r) => r !== 'HEAD').map((r) => h('span', { class: 'hi-ref' }, r)),
      h('span', { class: 'hc-sp' }),
      fold),
    bodyEl, more,
    h('div', { class: 'hc-meta faint small' },
      h('button', { class: 'link-btn num', 'data-tip': 'Copy the full sha', on: { click: () => copy(d.sha) } }, d.short),
      h('span', null, ` · ${d.author} · `),
      h('span', { title: d.date }, relTime(d.date)),
      d.parents.length ? h('span', null, ' · parent ', d.parents.map((p, i) => h('span', null, i ? ', ' : '',
        h('button', { class: 'link-btn num', 'data-tip': 'Open the parent commit', on: { click: () => showCommit(p, ctx) } }, p.slice(0, 7))))) : h('span', null, ' · first commit'),
      h('span', { class: 'hc-sp' }),
      h('button', { class: 'btn ghost sm', on: { click: () => compare(ctx, { base: d.sha, target: 'worktree' }) } }, icon('git-compare', 'sm'), 'Compare with working tree')));
  function setFolded(min, save) {
    bar.classList.toggle('hc-min', min);
    const tip = min ? 'Show the full commit message' : 'Show only the summary';
    fold.setAttribute('aria-expanded', String(!min));
    fold.setAttribute('aria-label', tip);
    fold.dataset.tip = tip;
    mount(fold, icon(min ? 'chevron-down' : 'chevron-up', 'sm'));
    if (save) try { localStorage.setItem(FOLD_KEY, min ? 'min' : 'full'); } catch { /* private mode */ }
  }
  setFolded(foldedPref(), false);
  dv.setBanner(bar);
  offs.push(bus.on('diff:closed', end), bus.on('diff:open', end));
  ctx.closeSidebar?.();
  await dv.show({ base: d.base, target: d.sha, baseLabel: d.parents[0] ? d.parents[0].slice(0, 7) : 'empty tree' });
}

/** Compare any two revisions (`target` may be `worktree`). */
export async function compare(ctx, { base, target }) {
  const dv = await ctx.getDiffView();
  endCommitView?.();
  const label = (r) => (r === 'worktree' ? 'working tree' : short(r));
  const bar = h('div', { class: 'hc-bar', role: 'region', 'aria-label': 'Comparison' },
    h('div', { class: 'hc-top' }, icon('git-compare', 'sm'),
      h('span', { class: 'hc-subject' }, 'Comparing ', h('code', null, label(base)), ' → ', h('code', null, label(target))),
      h('span', { class: 'hc-sp' }),
      target !== 'worktree' ? h('button', { class: 'btn ghost sm', 'data-tip': 'Swap the two sides', on: { click: () => compare(ctx, { base: target, target: base }) } }, 'Swap') : null,
      h('button', { class: 'btn ghost sm', on: { click: () => openCompareDialog(ctx, { base, target }) } }, 'Change…')));
  const offs = [];
  const end = () => { endCommitView = null; for (const off of offs) off(); dv.setBanner(null); };
  endCommitView = end;
  dv.setBanner(bar);
  offs.push(bus.on('diff:closed', end), bus.on('diff:open', end));
  ctx.closeSidebar?.();
  await dv.show({ base, target, baseLabel: label(base) });
}

function copy(text) {
  navigator.clipboard?.writeText(text).then(
    () => toast({ kind: 'ok', title: 'Copied', message: text, timeout: 1500 }),
    () => toast({ kind: 'error', title: 'Could not copy', timeout: 2000 }),
  );
}

// ---------- compare dialog ----------

/** Pick two revisions: branches, tags, recent commits or anything git understands (HEAD~3). */
export async function openCompareDialog(ctx, { base = '', target = 'worktree' } = {}) {
  const refs = await loadRefs().catch(() => null);
  const recent = await request('git/log', { query: { limit: 30 } }).then((r) => r.commits).catch(() => []);
  const list = h('datalist', { id: 'hc-revs' },
    h('option', { value: 'HEAD' }, 'Current commit'),
    (refs?.branches || []).map((b) => h('option', { value: b.name }, `branch · ${b.subject}`)),
    (refs?.remotes || []).map((b) => h('option', { value: b.name }, `remote · ${b.subject}`)),
    (refs?.tags || []).slice(0, 200).map((t) => h('option', { value: t.name }, `tag · ${t.subject}`)),
    recent.map((c) => h('option', { value: c.short }, c.subject)));
  const from = h('input', { class: 'input', attrs: { list: 'hc-revs' }, value: base || refs?.head?.branch || 'HEAD', placeholder: 'main, v1.2, a1b2c3d, HEAD~3', spellcheck: 'false', 'aria-label': 'From (older)' });
  const worktree = target === 'worktree';
  const to = h('input', { class: 'input', attrs: { list: 'hc-revs' }, value: worktree ? '' : target, placeholder: 'Working tree (your current files)', spellcheck: 'false', 'aria-label': 'To (newer); empty compares with your current files' });
  const err = h('p', { class: 'agent-error', role: 'alert', hidden: true });
  const swap = h('button', { class: 'icon-btn sm', type: 'button', 'aria-label': 'Swap', 'data-tip': 'Swap', on: { click: () => { const a = from.value; from.value = to.value || 'HEAD'; to.value = a; } } }, icon('arrow-down', 'sm'));
  const run = async () => {
    const b = from.value.trim();
    const t = to.value.trim() || 'worktree';
    if (!b) { err.textContent = 'Choose what to compare from.'; err.hidden = false; return false; }
    compare(ctx, { base: b, target: t }).catch(() => {});
    return true;
  };
  const dlg = openDialog({
    title: 'Compare',
    className: 'hc-dialog',
    width: 'min(520px, calc(100vw - 32px))',
    body: h('div', { class: 'hc-form' }, list,
      h('label', { class: 'hc-field' }, h('span', { class: 'faint small' }, 'From'), from),
      h('div', { class: 'hc-swap' }, swap),
      h('label', { class: 'hc-field' }, h('span', { class: 'faint small' }, 'To'), to),
      err,
      h('p', { class: 'faint small' }, 'A branch, tag, commit, or anything git understands (HEAD~3). Leave To empty to compare with your current files.')),
    actions: [{ label: 'Cancel' }, { label: 'Compare', primary: true, run }],
  });
  for (const input of [from, to]) {
    input.addEventListener('keydown', (e) => { if (e.key === 'Enter') { e.preventDefault(); run().then((ok) => { if (ok) dlg.close(); }); } });
  }
  from.select();
}

// ---------- switching branches ----------

/** Switch to a branch, remote branch, tag or commit after a confirm. Never over local changes. */
export function confirmCheckout(rev, { kind = 'branch' } = {}) {
  const g = store.get('git');
  const dirty = g ? g.counts.staged + g.counts.unstaged + g.counts.conflicted : 0;
  const detached = kind === 'tag' || kind === 'commit';
  openDialog({
    title: `Switch to ${short(rev)}?`,
    body: h('div', null,
      h('p', null, detached
        ? `Your files change to how they are at ${short(rev)}. You will be on no branch (detached); switch back to a branch any time.`
        : kind === 'remote'
          ? `Your files change to ${rev}. A local branch tracking it is created if you do not have one.`
          : `Your files change to the ${rev} branch.`),
      dirty ? h('p', { class: 'agent-error' }, `You have uncommitted changes in ${plural(dirty, 'file')}. Commit or stash them first; ferro will not switch over them.`) : null),
    actions: [{ label: 'Cancel' }, {
      label: 'Switch',
      primary: true,
      run: async () => {
        try {
          const res = await request('git/checkout', { method: 'POST', body: { ref: rev } });
          store.set('git', res.status);
          refsCache = null;
          toast({ kind: 'ok', title: res.status.branch ? `On ${res.status.branch}` : `At ${short(rev)} (detached)`, timeout: 2500 });
          bus.emit('history:refresh');
        } catch (e) {
          toast({ kind: 'error', title: e.status === 409 ? 'Commit or stash your changes first' : 'Could not switch', message: e.message });
        }
        return true;
      },
    }],
  });
}

/** Switch Branch…: a filterable list of local and remote branches. */
export async function openBranchPicker() {
  let refs;
  try { refs = await loadRefs(true); } catch (e) { toast({ kind: 'error', title: 'Could not list branches', message: e.message }); return; }
  const filter = h('input', { class: 'input', placeholder: 'Filter branches', spellcheck: 'false', 'aria-label': 'Filter branches' });
  const listEl = h('div', { class: 'hb-list', role: 'listbox', 'aria-label': 'Branches' });
  const all = [
    ...refs.branches.map((b) => ({ ...b, kind: 'branch' })),
    ...refs.remotes.filter((r) => !refs.branches.some((b) => r.name.endsWith(`/${b.name}`))).map((b) => ({ ...b, kind: 'remote' })),
  ];
  let dlg = null;
  const render = () => {
    const q = filter.value.trim().toLowerCase();
    const shown = all.filter((b) => !q || b.name.toLowerCase().includes(q)).slice(0, 200);
    mount(listEl, shown.length ? shown.map((b) => h('button', {
      class: `hb-row${b.current ? ' current' : ''}`,
      role: 'option',
      disabled: b.current,
      on: { click: () => { dlg.close(); confirmCheckout(b.name, { kind: b.kind }); } },
    }, icon('git-branch', 'sm'), h('span', { class: 'hb-name' }, b.name),
    b.kind === 'remote' ? h('span', { class: 'hi-ref' }, 'remote') : null,
    b.current ? h('span', { class: 'hi-ref' }, 'current') : null,
    h('span', { class: 'hb-sub faint small truncate' }, b.subject),
    h('span', { class: 'faint small' }, b.date ? relTime(b.date) : ''))) : h('p', { class: 'faint small' }, 'No branch matches.'));
  };
  filter.addEventListener('input', render);
  filter.addEventListener('keydown', (e) => { if (e.key === 'Enter') listEl.querySelector('.hb-row:not([disabled])')?.click(); });
  render();
  dlg = openDialog({ title: 'Switch branch', className: 'hb-dialog', width: 'min(560px, calc(100vw - 32px))', body: h('div', { class: 'hb' }, filter, listEl) });
  filter.focus();
}

// ---------- the History panel ----------

export function renderHistoryPanel(section, ctx) {
  const refSel = h('select', { class: 'input sm hi-ref-sel', 'aria-label': 'Branch or tag' });
  const switchBtn = h('button', { class: 'icon-btn sm', hidden: true, 'aria-label': 'Switch to this branch', 'data-tip': 'Switch to this branch', on: { click: () => { const o = refSel.selectedOptions[0]; if (o?.value) confirmCheckout(o.value, { kind: o.dataset.kind }); } } }, icon('git-branch', 'sm'));
  const search = h('input', { class: 'input sm hi-search', type: 'search', placeholder: 'Search messages · @author', spellcheck: 'false', 'aria-label': 'Search commits' });
  const info = h('div', { class: 'hi-info faint small', role: 'status' });
  const scroller = h('div', { class: 'hi-list', role: 'listbox', 'aria-label': 'Commits', tabindex: '0' });
  mount(section,
    h('div', { class: 'panel-head' },
      h('span', { class: 'panel-title' }, 'History'),
      h('div', { class: 'panel-actions' },
        h('button', { class: 'icon-btn sm', 'aria-label': 'Compare…', 'data-tip': 'Compare two revisions', on: { click: () => openCompareDialog(ctx) } }, icon('git-compare', 'sm')),
        h('button', { class: 'icon-btn sm hi-fetch', 'aria-label': 'Fetch from remotes', 'data-tip': 'Fetch all branches and tags', on: { click: () => fetchAll() } }, icon('arrow-down', 'sm')),
        h('button', { class: 'icon-btn sm', 'aria-label': 'Refresh history', 'data-tip': 'Refresh', on: { click: () => reload(true) } }, icon('refresh', 'sm')))),
    h('div', { class: 'panel-body hi-body' },
      h('div', { class: 'hi-bar' }, refSel, switchBtn),
      search, info, scroller));

  let commits = [];
  let hasMore = false;
  let loading = false;
  let seq = 0;
  let selected = null; // sha open in the diff view
  let pick = null; // first commit of a Cmd/Ctrl-click comparison
  let headSha = store.get('git')?.headSha || null;

  const vl = new VirtualList({
    scroller,
    rowHeight: ROW_H,
    overscan: 8,
    create: () => h('div', { class: 'hi-row', role: 'option' }),
    update: (el, i) => {
      const c = commits[i];
      if (!c) return;
      el.__sha = c.sha;
      el.classList.toggle('selected', c.sha === selected);
      el.classList.toggle('picked', c.sha === pick);
      el.setAttribute('aria-selected', String(c.sha === selected));
      el.title = `${c.subject}\n${c.short} · ${c.author} <${c.email}> · ${c.date}`;
      mount(el,
        h('div', { class: 'hi-line1' },
          c.parents.length > 1 ? h('span', { class: 'hi-merge', title: 'Merge commit' }, icon('git-pull-request', 'xs')) : null,
          h('span', { class: 'hi-subj' }, c.subject),
          // `origin/x` adds nothing next to `x`: show each branch once, the local name first.
          shownRefs(c.refs).slice(0, 2).map((r) => h('span', { class: `hi-ref${c.refs.includes('HEAD') && r === store.get('git')?.branch ? ' head' : ''}`, title: r }, r))),
        h('div', { class: 'hi-line2 faint small' },
          h('span', { class: 'num' }, c.short), ` · ${c.author} · ${relTime(c.date)}`,
          h('span', { class: 'hi-actions' },
            h('button', { class: 'icon-btn xs', 'aria-label': 'Compare with working tree', 'data-tip': 'Compare with working tree', 'data-act': 'wt' }, icon('git-compare', 'xs')),
            h('button', { class: 'icon-btn xs', 'aria-label': 'Copy sha', 'data-tip': 'Copy sha', 'data-act': 'copy' }, icon('copy', 'xs')))));
    },
    onRange: (first, last) => { if (last >= commits.length - 20) more(); },
  });

  scroller.addEventListener('click', (e) => {
    const row = e.target.closest('.hi-row');
    const c = row && commits.find((x) => x.sha === row.__sha);
    if (!c) return;
    const act = e.target.closest('[data-act]')?.dataset.act;
    if (act === 'wt') { compare(ctx, { base: c.sha, target: 'worktree' }); return; }
    if (act === 'copy') { copy(c.sha); return; }
    if ((isMac ? e.metaKey : e.ctrlKey) || e.shiftKey) {
      // Second pick: compare the older (base) with the newer (target).
      if (pick && pick !== c.sha) {
        const a = commits.findIndex((x) => x.sha === pick);
        const b = commits.findIndex((x) => x.sha === c.sha);
        const [older, newer] = a > b ? [pick, c.sha] : [c.sha, pick];
        pick = null;
        selected = null;
        vl.refresh();
        compare(ctx, { base: older, target: newer });
      } else {
        pick = c.sha;
        vl.refresh();
        toast({ title: `Picked ${c.short}`, message: `${isMac ? '⌘' : 'Ctrl'}-click another commit to compare the two.`, timeout: 2500 });
      }
      return;
    }
    pick = null;
    selected = c.sha;
    vl.refresh();
    showCommit(c.sha, ctx);
  });
  scroller.addEventListener('keydown', (e) => {
    if (e.key !== 'ArrowDown' && e.key !== 'ArrowUp' && e.key !== 'Enter') return;
    e.preventDefault();
    let i = commits.findIndex((x) => x.sha === selected);
    if (e.key === 'ArrowDown') i = Math.min(commits.length - 1, i + 1);
    else if (e.key === 'ArrowUp') i = Math.max(0, i - 1);
    const c = commits[i];
    if (!c) return;
    selected = c.sha;
    const top = i * ROW_H;
    if (top < scroller.scrollTop) scroller.scrollTop = top;
    else if (top + ROW_H > scroller.scrollTop + scroller.clientHeight) scroller.scrollTop = top + ROW_H - scroller.clientHeight;
    vl.refresh();
    showCommit(c.sha, ctx);
  });

  function query(skip) {
    const v = refSel.value;
    const s = search.value.trim();
    const author = s.startsWith('@') ? s.slice(1).trim() : '';
    return {
      limit: PAGE,
      skip,
      ...(v === '__all__' ? { all: 1 } : v ? { rev: v } : {}),
      ...(author ? { author } : s ? { q: s } : {}),
    };
  }

  async function load() {
    const my = ++seq;
    loading = true;
    info.textContent = 'Loading…';
    try {
      const res = await request('git/log', { query: query(0) });
      if (my !== seq) return;
      commits = res.commits;
      hasMore = res.hasMore;
      scroller.scrollTop = 0;
      vl.setCount(commits.length);
      setInfo();
    } catch (e) {
      if (my !== seq) return;
      commits = [];
      vl.setCount(0);
      info.textContent = e.status === 422 ? 'Not a git repository.' : e.message;
    } finally {
      if (my === seq) loading = false;
    }
  }

  async function more() {
    if (loading || !hasMore) return;
    const my = seq;
    loading = true;
    try {
      const res = await request('git/log', { query: query(commits.length) });
      if (my !== seq) return;
      commits = commits.concat(res.commits);
      hasMore = res.hasMore;
      vl.setCount(commits.length);
      setInfo();
    } catch { /* keep what is shown */ } finally {
      if (my === seq) loading = false;
    }
  }

  function setInfo() {
    const s = search.value.trim();
    info.textContent = commits.length
      ? `${hasMore ? `${formatCount(commits.length)}+ commits` : plural(commits.length, 'commit')}${s ? ' matching' : ''}`
      : s ? 'No commit matches.' : 'No commits yet.';
  }

  async function fillRefs(force = false) {
    let refs;
    try { refs = await loadRefs(force); } catch { return; }
    const keep = refSel.value;
    const cur = refs.head.branch;
    const opt = (value, label, kind) => h('option', { value, 'data-kind': kind }, label);
    mount(refSel,
      opt('', cur ? `${cur} (current)` : `HEAD (detached at ${short(refs.head.sha || '')})`, ''),
      opt('__all__', 'All branches', ''),
      refs.branches.length ? h('optgroup', { label: 'Branches' }, refs.branches.filter((b) => !b.current).map((b) => opt(b.name, b.name, 'branch'))) : null,
      refs.remotes.length ? h('optgroup', { label: 'Remote branches' }, refs.remotes.map((b) => opt(b.name, b.name, 'remote'))) : null,
      refs.tags.length ? h('optgroup', { label: 'Tags' }, refs.tags.slice(0, 300).map((t) => opt(t.name, t.name, 'tag'))) : null);
    refSel.value = [...refSel.options].some((o) => o.value === keep) ? keep : '';
    syncSwitch();
  }

  function syncSwitch() {
    const o = refSel.selectedOptions[0];
    switchBtn.hidden = !o?.value || o.value === '__all__';
  }

  async function reload(force = false) {
    await fillRefs(force);
    await load();
  }

  async function fetchAll() {
    const btn = section.querySelector('.hi-fetch');
    btn.disabled = true;
    try {
      const res = await request('git/fetch', { method: 'POST', body: {} });
      store.set('git', res.status);
      toast({ kind: 'ok', title: 'Fetched', message: res.output?.trim() || 'Everything is up to date.', timeout: 2500 });
      await reload(true);
    } catch (e) {
      toast({ kind: 'error', title: 'Fetch failed', message: e.detail?.stderr || e.message });
    } finally {
      btn.disabled = false;
    }
  }

  refSel.addEventListener('change', () => { syncSwitch(); load(); });
  search.addEventListener('input', debounce(load, 250));
  // A commit, checkout or pull moved HEAD: the list and the refs follow.
  store.subscribe('git', (g) => {
    if (!g || g.headSha === headSha) return;
    headSha = g.headSha;
    refsCache = null;
    reload();
  });
  bus.on('history:refresh', () => reload(true));
  reload();
  return { focus: () => search.focus() };
}
