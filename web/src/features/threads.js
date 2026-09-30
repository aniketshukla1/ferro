// Agent threads (API.md § 10.7): saved multi-turn conversations with the opted-in coding agent,
// in the inspector's Agent tab. Each turn runs the harness; its changes are reviewed and reverted
// like an Alt+E edit (agent.js). Review drafts go to a new thread as one batch.
import { h, mount } from '../core/dom.js';
import { request } from '../core/api.js';
import { bus } from '../core/bus.js';
import { store } from '../core/store.js';
import { basename, formatMs, isMac, plural } from '../core/util.js';
import { keysEl } from '../core/keys.js';
import { icon } from '../ui/icons.js';
import { toast } from '../ui/overlay.js';
import { openDialog } from '../ui/dialog.js';

let panel = null;
let pending = null; // a batch queued before the tab first rendered

/** Batch agent edit: one new thread with every draft as context. */
export function sendDraftsToAgent(drafts) {
  startAgentTask({
    title: `Address ${plural(drafts.length, 'review comment')}`,
    message: 'Address these review comments. Keep each change minimal and say what you changed for each one.',
    context: drafts.map((d) => ({ path: d.path, startLine: d.startLine || d.line, endLine: d.line, note: d.body || '' })),
  });
}

/** A new thread with one turn: `{ title, message, context: [{ path, startLine?, endLine?, note }] }`. */
export function startAgentTask(batch) {
  bus.emit('threads:open');
  if (panel) panel.start(batch);
  else pending = batch;
}

/** The Agent tab. `ctx` = { editor, getDiffView }. */
export function renderThreadsTab(el, ctx) {
  const picker = h('select', { class: 'input sm th-pick', 'aria-label': 'Agent thread' });
  const newBtn = h('button', { class: 'icon-btn sm', 'aria-label': 'New thread', 'data-tip': 'New thread', on: { click: () => select(null) } }, icon('plus', 'sm'));
  const delBtn = h('button', { class: 'icon-btn sm', 'aria-label': 'Delete thread', 'data-tip': 'Delete thread', on: { click: () => remove() } }, icon('x', 'sm'));
  const agentLine = h('div', { class: 'th-agent faint small' });
  const log = h('div', { class: 'th-log', role: 'log', 'aria-live': 'polite', 'aria-label': 'Conversation' });
  const chips = h('div', { class: 'th-chips' });
  const input = h('textarea', { class: 'input th-input', rows: 3, placeholder: 'Ask the agent to change something…', 'aria-label': 'Message to the agent' });
  const sendBtn = h('button', { class: 'btn primary sm', on: { click: () => send() } }, 'Send', keysEl('Mod+Enter'));
  const stopBtn = h('button', { class: 'btn sm', hidden: true, on: { click: () => stop() } }, icon('stop', 'sm'), 'Stop');
  const selBtn = h('button', { class: 'btn ghost sm', 'data-tip': 'Attach the selected lines', on: { click: () => attachSelection() } }, icon('at', 'sm'), 'Selection');
  mount(el, h('div', { class: 'th' },
    h('div', { class: 'th-head' }, picker, newBtn, delBtn),
    agentLine, log,
    h('div', { class: 'th-compose' }, chips, input, h('div', { class: 'row th-actions' }, selBtn, h('span', { class: 'grow' }), stopBtn, sendBtn))));

  let threads = [];
  let current = null; // Thread
  let context = [];
  let harnessState = null;
  let tick = 0;

  input.addEventListener('keydown', (e) => {
    if (e.key === 'Enter' && (isMac ? e.metaKey : e.ctrlKey)) { e.preventDefault(); send(); }
  });
  picker.addEventListener('change', () => select(picker.value || null));

  async function refreshList(keep = current?.id) {
    try { threads = (await request('harness/threads')).threads || []; } catch { threads = []; }
    mount(picker, h('option', { value: '' }, 'New thread'), threads.map((t) => h('option', { value: t.id }, `${t.title} · ${plural(t.turns, 'turn')}`)));
    picker.value = keep || '';
  }

  async function select(id) {
    current = id ? await request(`harness/threads/${encodeURIComponent(id)}`).catch(() => null) : null;
    picker.value = current?.id || '';
    delBtn.disabled = !current;
    renderLog();
  }

  async function loadHarness() {
    try { harnessState = await request('harness'); } catch { harnessState = null; }
    const cur = harnessState?.harnesses?.find((x) => x.id === harnessState.selected);
    if (cur) {
      mount(agentLine, icon('terminal', 'xs'), ` ${cur.label}${harnessState.model ? ` · ${harnessState.model}` : ''}`);
      return;
    }
    // First use: an explicit choice before anything runs (API.md § 10.5).
    const installed = (harnessState?.harnesses || []).filter((x) => x.installed);
    mount(agentLine, installed.length
      ? h('span', null, 'Choose the coding agent ferro may run: ', installed.map((x) => h('button', {
        class: 'btn ghost sm',
        on: { click: async () => { await request('harness', { method: 'PUT', body: { id: x.id } }).catch((e) => toast({ kind: 'error', title: 'Could not choose that agent', message: e.message })); loadHarness(); } },
      }, x.label)))
      : 'No supported coding agent is installed where ferro runs.');
  }

  function running() { return current?.turns.find((t) => t.state === 'running') || null; }

  function renderLog() {
    const turns = current?.turns || [];
    mount(log, turns.length ? turns.map(renderTurn) : h('div', { class: 'empty th-empty' },
      h('p', null, 'Talk to your coding agent here. Every turn remembers the earlier ones, and each change can be reviewed hunk by hunk or reverted.')));
    const run = running();
    stopBtn.hidden = !run;
    sendBtn.disabled = !!run;
    clearInterval(tick);
    if (run) tick = setInterval(() => { const e = log.querySelector('.th-elapsed'); if (e) e.textContent = formatMs(Date.now() - Date.parse(run.at)); }, 1000);
    log.scrollTop = log.scrollHeight;
  }

  function renderTurn(t) {
    const r = t.result || {};
    const changed = r.changed || [];
    const label = harnessState?.harnesses?.find((x) => x.id === t.harness)?.label || t.harness;
    const status = t.state === 'running'
      ? h('span', { class: 'th-state' }, h('span', { class: 'spinner' }), ` ${label} is working · `, h('span', { class: 'th-elapsed num' }, '0s'))
      : h('span', { class: `th-state ${t.state}` }, `${label} ${t.state === 'done' ? 'finished' : t.state}`, r.ms != null ? ` · ${formatMs(r.ms)}` : '', r.exitCode ? ` · exit ${r.exitCode}` : '');
    const out = [r.stdoutTail, r.stderrTail].filter(Boolean).join('\n');
    return h('article', { class: 'th-turn' },
      h('div', { class: 'th-user' }, h('p', null, t.message),
        t.context?.length ? h('ul', { class: 'th-ctx' }, t.context.map((c) => h('li', { title: c.note || '' }, h('code', null, `${basename(c.path)}${c.startLine ? `:${c.startLine}${c.endLine && c.endLine !== c.startLine ? `–${c.endLine}` : ''}` : ''}`), c.note ? ` ${c.note.slice(0, 80)}` : ''))) : null),
      h('div', { class: 'th-agent-reply' }, status,
        t.error?.message ? h('p', { class: 'th-error' }, t.error.message) : null,
        changed.length ? h('div', { class: 'th-changed' },
          h('span', { class: 'faint small' }, `Changed ${plural(changed.length, 'file')}:`),
          changed.map((p) => h('button', { class: 'link-btn', title: p, on: { click: () => ctx.onOpen?.(p, { focus: true }) } }, basename(p))),
          h('div', { class: 'row' },
            h('button', { class: 'btn sm', on: { click: () => reviewTurn(t, label) } }, icon('git-compare', 'sm'), 'Review'),
            h('button', { class: 'btn ghost sm', on: { click: () => revertTurn(t) } }, icon('history', 'sm'), 'Revert'))) : t.state === 'done' ? h('p', { class: 'faint small' }, 'No files changed.') : null,
        out ? h('details', { class: 'th-out' }, h('summary', null, 'Output'), h('pre', null, out)) : null));
  }

  async function reviewTurn(t, label) {
    const m = await import('./agent.js');
    m.reviewEdit({ id: t.jobId, result: t.result, threadId: current.id, turnId: t.id }, label, ctx);
  }

  function revertTurn(t) {
    openDialog({
      title: 'Revert this turn?',
      body: h('p', null, `Restores ${plural(t.result.changed.length, 'file')} to how they were before this turn ran. Later turns that touched the same files lose those edits too.`),
      actions: [{ label: 'Cancel' }, {
        label: 'Revert',
        primary: true,
        run: async () => {
          try {
            const res = await request('harness/revert', { method: 'POST', body: { jobId: t.jobId, threadId: current.id, turnId: t.id } });
            toast({ kind: 'ok', title: `Reverted ${plural(res.reverted.length, 'file')}`, timeout: 2000 });
            bus.emit('git:refresh');
          } catch (e) {
            toast({ kind: 'error', title: 'Revert failed', message: e.message });
            return false;
          }
          return true;
        },
      }],
    });
  }

  function renderChips() {
    mount(chips, context.map((c, i) => h('span', { class: 'chip' }, h('code', null, `${basename(c.path)}${c.startLine ? `:${c.startLine}` : ''}`),
      h('button', { class: 'icon-btn xs', 'aria-label': 'Remove', on: { click: () => { context.splice(i, 1); renderChips(); } } }, icon('x', 'xs')))));
  }

  function attachSelection() {
    const sel = ctx.editor?.activeView()?.selection?.();
    if (!sel) { toast({ title: 'Open a file and select lines first', timeout: 2000 }); return; }
    context.push({ path: sel.path, startLine: Math.min(sel.start, sel.end), endLine: Math.max(sel.start, sel.end), note: '' });
    renderChips();
  }

  async function send(batch) {
    const message = (batch?.message ?? input.value).trim();
    if (!message) { input.focus(); return; }
    try {
      if (!current || batch) {
        current = await request('harness/threads', { method: 'POST', body: { title: batch?.title } });
      }
      const res = await request(`harness/threads/${encodeURIComponent(current.id)}/turns`, { method: 'POST', body: { message, context: batch?.context ?? context } });
      current.turns.push(res.turn);
      if (!batch) { input.value = ''; context = []; renderChips(); }
      await refreshList(current.id);
      current = await request(`harness/threads/${encodeURIComponent(current.id)}`);
      renderLog();
    } catch (e) {
      toast({ kind: 'error', title: e.status === 422 ? 'Choose a coding agent first' : 'Could not send', message: e.message });
    }
  }

  async function stop() {
    const run = running();
    if (run) await request(`jobs/${encodeURIComponent(run.jobId)}/cancel`, { method: 'POST', body: {} }).catch(() => {});
  }

  function remove() {
    if (!current) return;
    openDialog({
      title: 'Delete this thread?',
      body: h('p', null, 'The conversation is deleted. Files the agent changed stay as they are.'),
      actions: [{ label: 'Cancel' }, {
        label: 'Delete',
        primary: true,
        run: async () => {
          await request(`harness/threads/${encodeURIComponent(current.id)}`, { method: 'DELETE' }).catch((e) => toast({ kind: 'error', title: 'Could not delete', message: e.message }));
          await refreshList(null);
          select(null);
        },
      }],
    });
  }

  // A turn settled (or progressed): reload the thread it belongs to.
  const offJob = bus.on('ev:job', (job) => {
    if (!current || job?.progress?.threadId !== current.id || job.state === 'running') return;
    setTimeout(() => select(current.id).then(() => refreshList(current?.id)), 50);
  });
  store.subscribe('meta', () => loadHarness());

  panel = {
    start: (batch) => send(batch),
    destroy() { offJob(); clearInterval(tick); },
  };
  (async () => {
    await loadHarness();
    await refreshList(null);
    await select(threads[0]?.id || null);
    if (pending) { const b = pending; pending = null; send(b); }
  })();
  return panel;
}
