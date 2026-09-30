// Edit with agent (FRONTEND.md § 6.14, API.md § 10.5): Alt+E on a selection → composer →
// `harness.edit` job → the diff since the run's snapshot, with Revert per hunk (`/git/hunk`),
// Revert all (`/harness/revert`) and Keep. Loads on first use.
import { h, mount } from '../core/dom.js';
import { request, has } from '../core/api.js';
import { bus } from '../core/bus.js';
import { basename, formatMs, isMac, plural } from '../core/util.js';
import { keysEl } from '../core/keys.js';
import { icon } from '../ui/icons.js';
import { toast } from '../ui/overlay.js';
import { openDialog } from '../ui/dialog.js';

const running = new Map(); // jobId -> { path, start, end }
const overlaps = (a, b) => a.path === b.path && a.start <= b.end && b.start <= a.end;

/**
 * Open the composer for the active code view's selection (or caret line).
 * @param {{editor:object, getDiffView:() => Promise<object>}} ctx
 */
export async function editWithAgent(ctx) {
  const view = ctx.editor.activeView();
  if (view?.kind !== 'code') { toast({ title: 'Open a file and select the lines to edit', timeout: 2500 }); return; }
  const sel = view.selection();
  const range = { path: sel.path, start: Math.min(sel.start, sel.end), end: Math.max(sel.start, sel.end) };
  // Refused here before the server sees it (the server also answers 409).
  for (const r of running.values()) {
    if (overlaps(r, range)) {
      toast({ kind: 'warn', title: 'An agent is already editing these lines', message: `${basename(r.path)} · lines ${r.start}–${r.end}` });
      return;
    }
  }
  let state;
  try {
    state = await request('harness');
  } catch (e) {
    toast({ kind: 'error', title: 'Cannot load coding agents', message: e.message });
    return;
  }
  openComposer(range, state, ctx);
}

function openComposer(range, state, ctx) {
  const lines = range.start === range.end ? `line ${range.start}` : `lines ${range.start}–${range.end}`;
  const instruction = h('textarea', {
    class: 'input agent-instruction', rows: 4, autofocus: true, spellcheck: 'true',
    placeholder: 'Describe the change, e.g. "handle the empty-input case and add a test"',
    'aria-label': 'Instruction for the agent',
  });
  const installed = state.harnesses.filter((x) => x.installed);
  let chosen = state.selected || (installed.length === 1 ? installed[0].id : null);
  const model = h('input', { class: 'input sm agent-model', value: state.model || '', placeholder: 'Harness default', 'aria-label': 'Model (optional)', spellcheck: 'false' });
  const picker = h('div', { class: 'agent-harnesses', role: 'radiogroup', 'aria-label': 'Coding agent' },
    state.harnesses.map((x) => h('label', { class: `agent-harness${x.installed ? '' : ' missing'}` },
      h('input', {
        type: 'radio', name: 'agent-harness', value: x.id, checked: x.id === chosen, disabled: !x.installed,
        on: { change: () => { chosen = x.id; err.hidden = true; } },
      }),
      h('span', { class: 'agent-harness-label' }, x.label),
      x.installed ? null : h('span', { class: 'faint small' }, 'not installed'))));
  // First use is an explicit choice (API.md § 10.5): nothing runs until a harness is picked here.
  const firstUse = !state.selected;
  const current = state.harnesses.find((x) => x.id === state.selected);
  const changeBtn = h('button', { class: 'btn ghost sm', type: 'button', on: { click: () => { pickRow.hidden = false; changeRow.hidden = true; } } }, 'Change');
  const changeRow = h('div', { class: 'agent-current', hidden: firstUse }, icon('terminal', 'sm'), h('span', null, current?.label || ''), changeBtn);
  const pickRow = h('div', { class: 'agent-pick', hidden: !firstUse },
    firstUse ? h('p', { class: 'faint small' }, 'Choose the coding agent ferro may run. It runs only after you pick it here; you can change it later.') : null,
    installed.length ? picker : h('p', { class: 'agent-none' }, 'No supported coding agent is installed (Claude Code, Codex, Gemini, Cursor Agent, OpenCode, Aider, Goose, Antigravity).'));
  const err = h('p', { class: 'agent-error', role: 'alert', hidden: true });
  const body = h('div', { class: 'agent-composer' },
    h('div', { class: 'agent-target' }, icon('file-code', 'sm'), h('span', { class: 'truncate' }, range.path), h('span', { class: 'faint' }, `· ${lines}`)),
    instruction,
    changeRow,
    pickRow,
    h('label', { class: 'agent-model-row' }, h('span', { class: 'faint small' }, 'Model'), model),
    err,
    h('p', { class: 'faint small agent-note' }, 'ferro snapshots your worktree first; you review every change and can revert it hunk by hunk. ', keysEl('Mod+Enter'), ' runs.'));

  async function run() {
    const text = instruction.value.trim();
    if (!text) { showErr('Write an instruction first.'); instruction.focus(); return false; }
    if (!chosen) { showErr('Choose a coding agent.'); return false; }
    try {
      // Record the choice (and model) when it differs from the saved one: the explicit opt-in.
      if (chosen !== state.selected || model.value.trim() !== (state.model || '')) {
        await request('harness', { method: 'PUT', body: { id: chosen, model: model.value.trim() } });
      }
      const res = await request('harness/edit', {
        method: 'POST',
        body: { path: range.path, startLine: range.start, endLine: range.end, instruction: text, harness: chosen, model: model.value.trim() || undefined },
      });
      track(res.job.id, range, state.harnesses.find((x) => x.id === chosen)?.label || chosen, ctx);
      return true;
    } catch (e) {
      showErr(e.status === 409 ? 'An agent is already editing these lines.' : e.message);
      return false;
    }
  }
  function showErr(msg) { err.textContent = msg; err.hidden = false; }

  const dlg = openDialog({
    title: 'Edit with agent',
    body,
    className: 'agent-dialog',
    width: 'min(560px, calc(100vw - 32px))',
    actions: [{ label: 'Cancel' }, { label: 'Run agent', primary: true, run }],
  });
  instruction.focus();
  instruction.addEventListener('keydown', (e) => {
    if (e.key === 'Enter' && (isMac ? e.metaKey : e.ctrlKey)) {
      e.preventDefault();
      run().then((ok) => { if (ok) dlg.close(); });
    }
  });
}

// ---------- job progress ----------

function track(jobId, range, label, ctx) {
  running.set(jobId, range);
  const t0 = performance.now();
  const lines = range.start === range.end ? `line ${range.start}` : `lines ${range.start}–${range.end}`;
  const progress = toast({
    title: `${label} is editing ${basename(range.path)}`,
    message: `${lines} · starting…`,
    timeout: 0,
    action: { label: 'Cancel', run: () => request(`jobs/${encodeURIComponent(jobId)}/cancel`, { method: 'POST', body: {} }).catch(() => {}) },
  });
  let settled = false;
  const tick = setInterval(() => progress.update({ message: `${lines} · ${formatMs(performance.now() - t0)}` }), 1000);
  const onJob = (job) => {
    if (job?.id !== jobId || settled) return;
    if (job.state === 'running' || job.state === 'queued') {
      if (job.progress?.message) progress.update({ message: `${lines} · ${job.progress.message}` });
      return;
    }
    settled = true;
    clearInterval(tick);
    clearInterval(poll);
    off();
    running.delete(jobId);
    progress.close();
    finish(job, label, ctx);
  };
  const off = bus.on('ev:job', onJob);
  // Events can be missed across a reconnect: poll as a fallback while the job runs.
  const poll = setInterval(() => request(`jobs/${encodeURIComponent(jobId)}`).then(onJob).catch(() => {}), 3000);
}

function finish(job, label, ctx) {
  const r = job.result || {};
  const output = () => showOutput(label, r);
  if (job.state === 'cancelled') { toast({ title: 'Agent edit cancelled', message: r.changed?.length ? `${plural(r.changed.length, 'file')} changed before it stopped` : undefined, action: r.changed?.length ? { label: 'Review', run: () => review(job, label, ctx) } : undefined }); return; }
  if (job.state === 'failed' || r.timedOut) {
    toast({
      kind: 'error',
      title: r.timedOut ? 'Agent timed out after 10 minutes' : 'Agent edit failed',
      message: job.error?.message || (r.exitCode != null ? `exit code ${r.exitCode}` : ''),
      action: r.changed?.length ? { label: 'Review changes', run: () => review(job, label, ctx) } : (r.stderrTail || r.stdoutTail) ? { label: 'Show output', run: output } : undefined,
    });
    return;
  }
  if (!r.changed?.length) {
    toast({ title: `${label} made no changes`, message: `Finished in ${formatMs(r.ms || 0)}`, action: (r.stdoutTail || r.stderrTail) ? { label: 'Show output', run: output } : undefined });
    return;
  }
  review(job, label, ctx);
}

function showOutput(label, r) {
  const pre = (title, text, cut) => (text ? h('section', { class: 'agent-output' }, h('h3', null, title, cut ? h('span', { class: 'faint small' }, ' (last 32 KiB)') : null), h('pre', null, text)) : null);
  openDialog({
    title: `${label} output`,
    body: h('div', null, pre('stdout', r.stdoutTail, r.stdoutTruncated), pre('stderr', r.stderrTail, r.stderrTruncated), (!r.stdoutTail && !r.stderrTail) ? h('p', { class: 'faint' }, 'No output.') : null),
    width: 'min(760px, calc(100vw - 32px))',
    actions: [{ label: 'Close', primary: true }],
  });
}

// ---------- review ----------

let endReview = null; // ends the review on screen (one at a time)

/**
 * Review a finished harness edit in the diff view (Keep / Revert all / per-hunk Revert).
 * @param {{id:string, result:object, threadId?:string, turnId?:string}} job
 */
export function reviewEdit(job, label, ctx) {
  return review(job, label, ctx);
}

async function review(job, label, ctx) {
  const r = job.result;
  const dv = await ctx.getDiffView();
  endReview?.(null);
  let ended = false;
  let offClose = null;
  const done = (msg) => {
    if (ended) return;
    ended = true;
    endReview = null;
    offClose?.();
    dv.setBanner(null);
    dv.setHunkAction(null);
    if (msg) toast({ kind: 'ok', title: msg, timeout: 2500 });
  };
  endReview = done;
  const revertAll = () => openDialog({
    title: 'Revert the agent edit?',
    body: h('p', null, `Restores ${plural(r.changed.length, 'file')} to how they were before ${label} ran, and deletes files it created. Your own edits in other files are untouched.`),
    actions: [{ label: 'Cancel' }, {
      label: 'Revert all',
      primary: true,
      run: async () => {
        try {
          // Thread turns also name themselves: a restarted server only has the turn on disk.
          const res = await request('harness/revert', { method: 'POST', body: { jobId: job.id, threadId: job.threadId, turnId: job.turnId } });
          done(`Reverted ${plural(res?.reverted?.length ?? r.changed.length, 'file')}`);
          dv.hide();
          bus.emit('git:refresh');
        } catch (e) {
          toast({ kind: 'error', title: 'Revert failed', message: e.message });
          return false;
        }
        return true;
      },
    }],
  });
  const bar = h('div', { class: 'agent-bar', role: 'region', 'aria-label': 'Agent edit' },
    icon('sparkles', 'sm'),
    h('span', { class: 'agent-bar-title' }, `${label} changed ${plural(r.changed.length, 'file')}`),
    h('span', { class: 'faint small' }, `${formatMs(r.ms || 0)}${r.exitCode ? ` · exit ${r.exitCode}` : ''}`),
    h('span', { class: 'agent-bar-sp' }),
    (r.stdoutTail || r.stderrTail) ? h('button', { class: 'btn ghost sm', on: { click: () => showOutput(label, r) } }, 'Output') : null,
    h('button', { class: 'btn sm', on: { click: revertAll } }, icon('history', 'sm'), 'Revert all'),
    h('button', { class: 'btn primary sm', on: { click: () => { done('Kept the agent’s changes'); dv.hide(); } } }, icon('check', 'sm'), 'Keep'));
  dv.setBanner(bar);
  if (has('git.hunk')) {
    dv.setHunkAction({
      label: 'Revert',
      run: async (path, hunk) => {
        try {
          await request('git/hunk', { method: 'POST', body: { path, hunkId: hunk.id, base: r.base, target: 'worktree', action: 'revert' } });
          await dv.reload();
          bus.emit('git:refresh');
        } catch (e) {
          toast({ kind: 'error', title: 'Could not revert that hunk', message: e.message });
        }
      },
    });
  }
  // Leaving the diff ends the review; the changes stay as they are (Keep).
  offClose = bus.on('diff:closed', () => done(null));
  await dv.show({ base: r.base, target: 'worktree', path: r.changed[0], baseLabel: 'Before the agent edit' });
}
