// Checks (API.md § 16, FRONTEND.md § 6.23): does this change break anything? One inspector tab
// for the diff that is open (working tree, commit, comparison, PR): breaking-change radar,
// affected tests, security scan, coverage of the changed lines, and agent-written tests.
// Checks that only read run by themselves; anything that executes project code runs on click,
// after showing the exact command. Loads on first use.
import { h, mount } from '../core/dom.js';
import { request, has } from '../core/api.js';
import { store } from '../core/store.js';
import { bus } from '../core/bus.js';
import { execute } from '../core/commands.js';
import { basename, formatMs, plural } from '../core/util.js';
import { icon } from '../ui/icons.js';
import { toast } from '../ui/overlay.js';
import { openDialog } from '../ui/dialog.js';

const SEV_ORDER = { high: 0, medium: 1, low: 2, info: 3 };
const isSha = (s) => /^[0-9a-f]{7,64}$/i.test(s || '');
const short = (r) => (r === 'worktree' ? 'working tree' : isSha(r) ? r.slice(0, 7) : r);

/** The pair the checks look at: the open diff, else the PR, else the working tree vs HEAD. */
function currentPair(ctx) {
  const dv = ctx.peekDiffView?.();
  if (dv && !dv.el.hidden) return { base: dv.getBase(), target: dv.getTarget() };
  const pr = store.get('pr');
  if (pr?.headSha) return { base: pr.mergeBaseSha || pr.baseSha || 'HEAD', target: pr.headSha };
  return { base: 'HEAD', target: 'worktree' };
}

/** Open `path` at `line` in the editor (callers, failures, findings). */
function openAt(ctx, path, line) {
  ctx.onOpen?.(path, { focus: true, line: line || undefined, preview: false });
}

function section(id, title, iconName, ...actions) {
  const status = h('span', { class: 'ck-status faint small' });
  const body = h('div', { class: 'ck-body' });
  const el = h('section', { class: 'ck-sec', 'data-check': id },
    h('div', { class: 'ck-head' }, icon(iconName, 'sm'), h('span', { class: 'ck-title' }, title), status, h('span', { class: 'ck-sp' }), ...actions),
    body);
  return { el, body, status };
}

// ---------- 1. breaking-change radar ----------

const CHANGE_TEXT = {
  removed: (c) => ['removed'],
  renamed: (c) => ['renamed to ', h('code', null, c.newName)],
  signature: () => ['signature changed'],
};

function radarSection(ctx) {
  const sec = section('breaking', 'Breaking changes', 'alert');
  let seq = 0;
  async function run(pair) {
    const my = ++seq;
    sec.status.textContent = 'checking…';
    mount(sec.body);
    let res;
    try {
      res = await request('checks/breaking', { query: pair });
    } catch (e) {
      if (my !== seq) return;
      sec.status.textContent = '';
      mount(sec.body, h('p', { class: 'faint small' }, e.status === 422 ? 'Not a git repository.' : e.message));
      return;
    }
    if (my !== seq) return;
    const risky = res.changes.filter((c) => c.severity === 'high' || c.severity === 'medium');
    sec.el.dataset.state = risky.length ? (res.changes.some((c) => c.severity === 'high') ? 'fail' : 'warn') : 'ok';
    sec.status.textContent = res.changes.length
      ? `${plural(risky.length, 'risk')} · ${plural(res.changes.length, 'API change')}`
      : `none in ${plural(res.scanned, 'file')}`;
    if (!res.changes.length) {
      mount(sec.body, h('p', { class: 'faint small' }, 'No function, type or method was removed, renamed or re-signatured.'));
      return;
    }
    mount(sec.body,
      res.indexed ? null : h('p', { class: 'faint small' }, 'The reference index is still building: caller counts may be missing.'),
      res.changes.sort((a, b) => SEV_ORDER[a.severity] - SEV_ORDER[b.severity]).map((c) => radarItem(ctx, c, pair)));
  }
  return { ...sec, run };
}

function radarItem(ctx, c, pair) {
  const refs = c.refs;
  const callers = refs.common
    ? 'name too common to list callers'
    : refs.count
      ? `${plural(refs.count, 'place')} still use${refs.count === 1 ? 's' : ''} it${refs.outsideFile ? ` (${refs.outsideFile} in other files)` : ''}`
      : c.change === 'signature' ? 'no callers found' : 'no callers left';
  const list = h('ul', { class: 'ck-refs', hidden: true },
    (refs.sample || []).map((r) => h('li', null, h('button', { class: 'link-btn', on: { click: () => openAt(ctx, r.path, r.line) } }, `${r.path}:${r.line}`))));
  const more = refs.sample?.length
    ? h('button', { class: 'link-btn small', on: { click: () => { list.hidden = !list.hidden; more.textContent = list.hidden ? 'Show callers' : 'Hide callers'; } } }, 'Show callers')
    : null;
  return h('div', { class: `ck-item sev-${c.severity}` },
    h('div', { class: 'ck-line' },
      h('span', { class: `ck-sev ${c.severity}`, title: c.severity }, c.severity === 'info' ? 'i' : c.severity[0].toUpperCase()),
      h('code', { class: 'ck-name' }, c.qualified),
      h('span', { class: 'faint small' }, c.kind, ' ', ...CHANGE_TEXT[c.change](c)),
      c.public ? h('span', { class: 'hi-ref' }, 'public') : null),
    h('div', { class: 'ck-sub faint small' },
      h('button', { class: 'link-btn', title: c.oldSignature, on: { click: () => bus.emit('diff:open', { ...pair, path: c.path }) } }, `${basename(c.path)}:${c.oldLine}`),
      ` · ${callers}`, c.stillDefined ? ' · another definition exists' : '', ' ', more),
    c.change === 'signature' ? h('div', { class: 'ck-sig' }, h('code', { class: 'del' }, c.oldSignature), h('code', { class: 'add' }, c.newSignature)) : null,
    list);
}

// ---------- 2. affected tests ----------

function testsSection(ctx) {
  const runBtn = h('button', { class: 'btn sm', disabled: true, on: { click: () => confirmRun() } }, icon('terminal', 'sm'), 'Run tests…');
  const stopBtn = h('button', { class: 'btn ghost sm', hidden: true, on: { click: () => stop() } }, icon('stop', 'sm'), 'Stop');
  const sec = section('tests', 'Tests', 'terminal', stopBtn, runBtn);
  let plan = null;
  let pair = null;
  let jobId = null;
  let seq = 0;

  async function run(p) {
    pair = p;
    const my = ++seq;
    if (jobId) return; // a run in progress keeps its results on screen
    sec.status.textContent = '';
    try {
      plan = await request('checks/tests/plan', { query: p });
    } catch (e) {
      if (my !== seq) return;
      plan = null;
      runBtn.disabled = true;
      mount(sec.body, h('p', { class: 'faint small' }, e.message));
      return;
    }
    if (my !== seq) return;
    delete sec.el.dataset.state;
    runBtn.disabled = !plan.steps.length || !plan.allowed;
    mount(sec.body, !plan.steps.length
      ? h('p', { class: 'faint small' }, 'No test runner recognized for these files (Cargo, Go, vitest / jest / npm test, pytest).')
      : [
        h('div', { class: 'ck-cmds' }, plan.steps.map((st) => h('div', { class: 'ck-cmd' }, h('code', null, (st.cwd ? `${st.cwd}$ ` : '$ ') + st.argv.join(' ')), h('span', { class: 'faint small' }, st.reason)))),
        plan.allowed ? null : h('p', { class: 'agent-error' }, plan.refusal),
      ]);
    sec.status.textContent = plan.steps.length ? plural(plan.steps.length, 'command') : '';
  }

  function confirmRun() {
    if (!plan?.steps.length) return;
    openDialog({
      title: 'Run the tests for this change?',
      body: h('div', { class: 'ck-confirm' },
        h('p', null, 'This runs your project’s code on this machine:'),
        plan.steps.map((st) => h('pre', { class: 'ck-out' }, `${st.cwd ? `cd ${st.cwd} && ` : ''}${st.argv.join(' ')}`)),
        plan.againstWorktree ? null : h('p', { class: 'faint small' }, `Tests run on your current files, not on ${short(pair.target)}.`)),
      actions: [{ label: 'Cancel' }, { label: 'Run', primary: true, run: () => start() }],
    });
  }

  async function start() {
    try {
      const res = await request('checks/tests/run', { method: 'POST', body: pair });
      jobId = res.job.id;
      runBtn.disabled = true;
      stopBtn.hidden = false;
      sec.el.dataset.state = '';
      sec.status.textContent = 'running…';
      mount(sec.body, h('p', { class: 'faint small ck-live' }, h('span', { class: 'spinner' }), ' starting…'));
    } catch (e) {
      toast({ kind: 'error', title: 'Could not run the tests', message: e.message });
    }
  }

  function stop() {
    if (jobId) request(`jobs/${encodeURIComponent(jobId)}/cancel`, { method: 'POST', body: {} }).catch(() => {});
  }

  const off = bus.on('ev:job', (job) => {
    if (!job || job.id !== jobId) return;
    if (job.state === 'running') {
      const live = sec.body.querySelector('.ck-live');
      if (live) mount(live, h('span', { class: 'spinner' }), ` ${job.progress?.message || 'running…'}`);
      return;
    }
    jobId = null;
    stopBtn.hidden = true;
    runBtn.disabled = !plan?.allowed;
    showResult(job);
  });

  function showResult(job) {
    const r = job.result;
    if (!r) {
      sec.status.textContent = job.state;
      mount(sec.body, h('p', { class: 'faint small' }, job.state === 'cancelled' ? 'Stopped.' : job.error?.message || 'The run did not finish.'));
      return;
    }
    sec.el.dataset.state = r.ok ? 'ok' : 'fail';
    sec.status.textContent = `${r.passed} passed · ${r.failed} failed${r.skipped ? ` · ${r.skipped} skipped` : ''}`;
    mount(sec.body, r.steps.map((st) => h('div', { class: 'ck-item' },
      h('div', { class: 'ck-line' },
        h('span', { class: `ck-sev ${st.exitCode === 0 ? 'ok' : 'high'}` }, st.exitCode === 0 ? '✓' : '✗'),
        h('code', { class: 'ck-name' }, st.command),
        h('span', { class: 'faint small' }, st.error ? st.error : `${st.passed} passed, ${st.failed} failed · ${formatMs(st.ms || 0)}${st.timedOut ? ' · timed out' : ''}${st.cancelled ? ' · stopped' : ''}`)),
      (st.failures || []).slice(0, 30).map((f) => h('div', { class: 'ck-sub ck-fail' },
        h('span', { class: 'ck-fname' }, f.name),
        f.path ? h('button', { class: 'link-btn', on: { click: () => openAt(ctx, f.path, f.line) } }, `${f.path}${f.line ? `:${f.line}` : ''}`) : null,
        f.message ? h('span', { class: 'faint small ck-msg' }, f.message) : null)),
      st.outputTail ? h('details', null, h('summary', { class: 'faint small' }, 'Output'), h('pre', { class: 'ck-out' }, st.outputTail)) : null)));
  }

  return { ...sec, run, destroy: off };
}

// ---------- 3. security ----------

const SEC_LETTER = { critical: 'C', high: 'H', medium: 'M', low: 'L' };
const SEC_RANK = { critical: 0, high: 1, medium: 2, low: 3 };

function securitySection(ctx) {
  const deepBtn = h('button', { class: 'btn sm', on: { click: () => confirmDeep() } }, icon('search', 'sm'), 'Deep scan…');
  const aiBtn = h('button', { class: 'btn ghost sm', hidden: !has('ai.review'), 'data-tip': 'AI review focused on security', on: { click: () => aiReview() } }, icon('sparkles', 'sm'), 'AI');
  const sec = section('security', 'Security', 'lock', aiBtn, deepBtn);
  const list = h('div', { class: 'ck-list' });
  const toolsEl = h('div', { class: 'ck-tools faint small' });
  let pair = null;
  let seq = 0;
  let builtin = [];
  let deep = null; // { findings, tools } of the last deep scan for this pair
  let tools = [];
  let jobId = null;

  async function run(p) {
    pair = p;
    deep = null;
    const my = ++seq;
    sec.status.textContent = 'scanning…';
    mount(sec.body, list, toolsEl);
    try {
      const res = await request('checks/security', { query: p });
      if (my !== seq) return;
      builtin = res.findings;
      tools = res.tools || [];
      render();
    } catch (e) {
      if (my !== seq) return;
      sec.status.textContent = '';
      mount(list, h('p', { class: 'faint small' }, e.message));
    }
  }

  function render() {
    const seen = new Set();
    const all = [...builtin, ...(deep?.findings || [])].filter((f) => {
      const k = `${f.path}:${f.line}:${f.rule}`;
      if (seen.has(k)) return false;
      seen.add(k);
      return true;
    }).sort((a, b) => SEC_RANK[a.severity] - SEC_RANK[b.severity]);
    // Team review memory hid some: counted apart, listed on request.
    const shown = all.filter((f) => !f.suppressedBy);
    const hidden = all.filter((f) => f.suppressedBy);
    const bad = shown.filter((f) => f.severity === 'critical' || f.severity === 'high').length;
    sec.el.dataset.state = bad ? 'fail' : shown.length ? 'warn' : 'ok';
    sec.status.textContent = (shown.length ? `${plural(shown.length, 'finding')}${bad ? ` · ${bad} serious` : ''}` : 'nothing found') + (hidden.length ? ` · ${hidden.length} ignored` : '');
    const item = (f) => h('div', { class: `ck-item sev-${f.severity}${f.suppressedBy ? ' ck-muted' : ''}` },
      h('div', { class: 'ck-line' },
        h('span', { class: `ck-sev ${f.severity}`, title: f.severity }, SEC_LETTER[f.severity] || '?'),
        h('span', { class: 'ck-ftitle' }, f.title),
        f.tool && f.tool !== 'ferro' ? h('span', { class: 'hi-ref' }, f.tool) : null,
        f.suppressedBy
          ? h('span', { class: 'hi-ref', title: f.suppressedBy.reason || '' }, f.suppressedBy.scope === 'team' ? 'ignored by team' : 'ignored by you')
          : has('memory') && !f.rule.startsWith('memory.') ? h('button', { class: 'link-btn small ck-ignore', 'data-tip': 'Don’t report this again', on: { click: () => ignore(f) } }, 'Ignore…') : null),
      h('div', { class: 'ck-sub faint small' },
        f.line ? h('button', { class: 'link-btn', on: { click: () => openFinding(f) } }, `${f.path}:${f.line}`) : h('span', null, f.path),
        h('span', { class: 'ck-msg' }, f.suppressedBy?.reason ? `Ignored: ${f.suppressedBy.reason}` : f.detail)),
      f.excerpt && !f.suppressedBy ? h('code', { class: 'ck-excerpt' }, f.excerpt) : null);
    const hiddenList = h('div', { class: 'ck-hidden', hidden: true }, hidden.map(item));
    mount(list,
      shown.length ? shown.map(item) : h('p', { class: 'faint small' }, 'No secrets or risky patterns in the added lines.'),
      hidden.length ? h('button', { class: 'link-btn small ck-show-hidden', on: { click: (e) => { hiddenList.hidden = !hiddenList.hidden; e.target.textContent = hiddenList.hidden ? `Show ${plural(hidden.length, 'ignored finding')}` : 'Hide ignored findings'; } } }, `Show ${plural(hidden.length, 'ignored finding')}`) : null,
      hiddenList);
    const t = deep?.tools || tools.map((x) => ({ name: x.name, status: x.installed ? 'installed' : 'missing' }));
    mount(toolsEl, deep ? 'Deep scan: ' : 'Deep-scan tools: ', t.map((x, i) => h('span', { title: x.detail || '' }, i ? ', ' : '', `${x.name} (${x.status})`)));
  }

  /** "Ignore…": a team-memory rule for this kind of finding; the scan re-runs with it. */
  async function ignore(f) {
    const m = await import('./memory.js');
    await m.openRuleDialog({ appliesTo: 'security', rule: f.rule, path: f.path });
  }

  function openFinding(f) {
    if (pair.target === 'worktree') openAt(ctx, f.path, f.line);
    else bus.emit('diff:open', { ...pair, path: f.path });
  }

  function confirmDeep() {
    const installed = tools.filter((t) => t.installed).map((t) => t.name);
    openDialog({
      title: 'Run a deep security scan?',
      body: h('div', { class: 'ck-confirm' },
        h('p', null, installed.length
          ? `Runs ${installed.join(', ')} on the changed files and lockfiles. They read files only and do not run your code, but may download advisory databases or rule packs.`
          : 'None of osv-scanner, cargo-audit, npm or semgrep is installed. Install one, then run the deep scan.'),
        h('p', { class: 'faint small' }, 'Only findings on lines this change adds (and advisories for changed lockfiles) are shown.')),
      actions: [{ label: 'Cancel' }, installed.length ? { label: 'Scan', primary: true, run: () => startDeep() } : null].filter(Boolean),
    });
  }

  async function startDeep() {
    try {
      const res = await request('checks/security/deep', { method: 'POST', body: pair });
      jobId = res.job.id;
      deepBtn.disabled = true;
      sec.status.textContent = 'deep scan running…';
    } catch (e) {
      toast({ kind: 'error', title: 'Could not start the scan', message: e.message });
    }
  }

  function aiReview() {
    store.set('aiReviewRequest', { base: pair.base, focus: ['security'] });
    execute('ai.review');
  }

  const off = bus.on('ev:job', (job) => {
    if (!job || job.id !== jobId) return;
    if (job.state === 'running') {
      sec.status.textContent = job.progress?.message || 'deep scan running…';
      return;
    }
    jobId = null;
    deepBtn.disabled = false;
    if (job.result) deep = job.result;
    else toast({ kind: 'error', title: 'Deep scan failed', message: job.error?.message || job.state });
    render();
  });
  // A rule was added, shared or deleted: the scan re-applies team memory.
  const offMem = bus.on('memory:changed', () => { if (pair) run(pair); });

  return { ...sec, run, destroy: () => { off(); offMem(); } };
}

// ---------- 5. coverage ----------

function coverageSection(ctx) {
  const showChk = h('input', { type: 'checkbox', 'aria-label': 'Show coverage in the diff', on: { change: () => sync() } });
  const sec = section('coverage', 'Coverage', 'activity', h('label', { class: 'ck-toggle faint small' }, showChk, 'In diff'));
  let pair = null;
  let res = null;
  let seq = 0;

  async function run(p) {
    pair = p;
    const my = ++seq;
    sec.status.textContent = 'reading…';
    try {
      res = await request('checks/coverage', { query: p });
    } catch (e) {
      if (my !== seq) return;
      res = null;
      sec.status.textContent = '';
      mount(sec.body, h('p', { class: 'faint small' }, e.message));
      return;
    }
    if (my !== seq) return;
    render();
    sync();
  }

  function render() {
    if (!res.report) {
      delete sec.el.dataset.state;
      sec.status.textContent = 'no report';
      mount(sec.body,
        h('p', { class: 'faint small' }, 'No coverage report found (lcov.info, coverage.xml, coverage.out). Produce one with your tests, then refresh:'),
        (res.hints || []).map((c) => h('code', { class: 'ck-excerpt' }, c)));
      return;
    }
    const t = res.totals;
    const newFiles = res.files.filter((f) => !f.inReport);
    sec.el.dataset.state = t.uncovered ? 'warn' : t.executable ? 'ok' : '';
    sec.status.textContent = t.executable ? `${t.covered} of ${plural(t.executable, 'added line')} ran · ${res.percent}%` : 'no executable added lines in the report';
    mount(sec.body,
      h('p', { class: 'faint small' }, `From ${res.report.path}`, res.stale ? ' · older than your latest edits: run coverage again' : ''),
      res.files.filter((f) => f.inReport && f.uncovered.length).map((f) => h('div', { class: 'ck-item' },
        h('div', { class: 'ck-line' }, h('span', { class: 'ck-sev medium' }, f.uncovered.length), h('code', { class: 'ck-name' }, f.path), h('span', { class: 'faint small' }, `${f.covered}/${f.added} ran`)),
        h('div', { class: 'ck-sub faint small' }, 'Never ran: ', f.uncovered.slice(0, 30).map((n, i) => h('span', null, i ? ', ' : '', h('button', { class: 'link-btn', on: { click: () => open(f.path, n) } }, String(n)))), f.uncovered.length > 30 ? ' …' : ''))),
      newFiles.length ? h('p', { class: 'faint small' }, `Not in the report (new, or never loaded by a test): ${newFiles.map((f) => f.path).join(', ')}`) : null);
  }

  function open(path, line) {
    if (pair.target === 'worktree') openAt(ctx, path, line);
    else bus.emit('diff:open', { ...pair, path });
  }

  /** Mark added lines in the open diff (red = never ran, green = ran). */
  function sync() {
    const dv = ctx.peekDiffView?.();
    if (!dv) return;
    if (!showChk.checked || !res?.report) { dv.setCoverage(null); return; }
    dv.setCoverage(new Map(res.files.filter((f) => f.inReport).map((f) => [f.path, { hit: new Set(f.coveredLines), miss: new Set(f.uncovered) }])));
  }

  // The diff may open (or be created) after the report was read.
  const off = bus.on('diff:shown', () => sync());
  return { ...sec, run, destroy: () => { off(); ctx.peekDiffView?.()?.setCoverage(null); } };
}

// ---------- 4. tests written by the coding agent ----------

const AGENT_KIND = {
  e2e: 'Write end-to-end tests with Playwright that exercise the behavior this change adds or alters through the app’s real UI (or its HTTP API if it has no UI). Reuse the project’s Playwright setup if it has one; otherwise add the smallest setup needed (one dev dependency, one config file). Cover the main path and at least one failure path for each changed behavior.',
  unit: 'Write unit and integration tests, in the project’s existing test framework and style, for the behavior this change adds or alters. Cover edge cases and at least one failure path.',
};

export function agentTestMessage(kind, pair) {
  return `${AGENT_KIND[kind]}\n\nThe change is ${short(pair.base)} → ${short(pair.target)}; the changed files are attached. Change only test files and test configuration, never product code. Run the tests you wrote. If a test fails because of a real bug in the change, keep the test and explain the bug. End with a short list: each test, what it checks, and whether it passes.`;
}

function agentSection() {
  const kind = h('select', { class: 'input sm', 'aria-label': 'Kind of tests' },
    h('option', { value: 'e2e' }, 'End-to-end (Playwright)'),
    h('option', { value: 'unit' }, 'Unit and integration'));
  const btn = h('button', { class: 'btn sm', on: { click: () => confirm() } }, icon('sparkles', 'sm'), 'Write tests…');
  const sec = section('agent', 'Tests by your agent', 'sparkles', kind, btn);
  let pair = null;
  mount(sec.body, h('p', { class: 'faint small' }, 'Your coding agent writes and runs tests for this change. Everything it does is snapshotted: review or revert it in the Agent tab.'));

  async function confirm() {
    let files = [];
    try {
      files = (await request('git/changes', { query: pair })).files.filter((f) => !f.binary && f.status !== 'D');
    } catch (e) {
      toast({ kind: 'error', title: 'Could not read the change', message: e.message });
      return;
    }
    if (!files.length) {
      toast({ title: 'Nothing to test', message: 'This change has no text files.', timeout: 2500 });
      return;
    }
    const harness = await request('harness').catch(() => null);
    const agent = harness?.harnesses?.find((x) => x.id === harness.selected);
    const message = agentTestMessage(kind.value, pair);
    const context = files.slice(0, 50).map((f) => ({ path: f.path, note: `${f.status} +${f.additions} -${f.deletions}` }));
    openDialog({
      title: 'Write tests with your agent?',
      body: h('div', { class: 'ck-confirm' },
        h('p', null, agent ? `${agent.label} gets this task and ${plural(context.length, 'changed file')}${files.length > 50 ? ' (the first 50)' : ''}:` : 'Choose a coding agent in the Agent tab first; it then gets this task:'),
        h('pre', { class: 'ck-out' }, message),
        h('p', { class: 'faint small' }, 'It can edit files and run commands in this folder. ferro snapshots the folder first, so every change can be reviewed hunk by hunk or reverted.')),
      actions: [{ label: 'Cancel' }, agent ? {
        label: 'Start',
        primary: true,
        run: async () => {
          (await import('./threads.js')).startAgentTask({ title: `Tests for ${short(pair.base)} → ${short(pair.target)}`, message, context });
        },
      } : { label: 'Choose an agent', primary: true, run: () => bus.emit('threads:open') }],
    });
  }

  return { ...sec, run: (p) => { pair = p; } };
}

// ---------- the tab ----------

export function renderChecksTab(el, ctx) {
  const pairEl = h('span', { class: 'ck-pair' });
  const rerun = h('button', { class: 'icon-btn sm', 'aria-label': 'Run the checks again', 'data-tip': 'Run again', on: { click: () => refresh(true) } }, icon('refresh', 'sm'));
  const sections = [
    radarSection(ctx),
    has('checks.tests') ? testsSection(ctx) : null,
    has('checks.security') ? securitySection(ctx) : null,
    has('harness.threads') ? agentSection(ctx) : null,
    has('checks.coverage') ? coverageSection(ctx) : null,
  ].filter(Boolean);
  mount(el, h('div', { class: 'ck' },
    h('div', { class: 'ck-top' }, icon('check-circle', 'sm'), pairEl, h('span', { class: 'ck-sp' }), rerun),
    sections.map((s) => s.el)));

  let pair = null;
  function refresh(force = false) {
    const next = currentPair(ctx);
    if (!force && pair && next.base === pair.base && next.target === pair.target) return;
    pair = next;
    mount(pairEl, h('code', null, short(pair.base)), ' → ', h('code', null, short(pair.target)));
    for (const s of sections) s.run?.(pair);
  }
  const offs = [
    bus.on('diff:shown', () => refresh()),
    bus.on('diff:closed', () => refresh()),
    // The working tree moved (save, commit, checkout): read-only checks follow it.
    bus.on('ev:git', () => { if (pair?.target === 'worktree') refresh(true); }),
  ];
  refresh(true);
  return { destroy: () => { offs.forEach((off) => off()); sections.forEach((s) => s.destroy?.()); }, refresh: () => refresh(true) };
}

export const _test = { currentPair, has };
