// Intent check (AI tab → Intent): say what the change should do, and ferro checks the change
// against it. Each requirement comes back done, partly done or missing, with the lines that show
// it, followed by changes the description does not mention, edge cases left open, and tests to
// add. The check covers the open diff, else the pull request, else the working tree.
import { h, mount } from '../core/dom.js';
import { request } from '../core/api.js';
import { bus } from '../core/bus.js';
import { store } from '../core/store.js';
import { basename } from '../core/util.js';
import { icon } from '../ui/icons.js';
import { toast } from '../ui/overlay.js';

const STAGES = { diff: 'Reading the change…', context: 'Finding where the changed code is used…', ai: 'Checking the change against your description…' };
const STATUS = { done: 'Done', partial: 'Partly', missing: 'Missing' };
const VERDICT = { complete: 'Does what you described', incomplete: 'Not finished yet', 'off-track': 'Does something else' };

/** The open diff, else the pull request, else the working tree against HEAD. */
function currentPair(ctx) {
  const dv = ctx.peekDiffView?.();
  if (dv && !dv.el.hidden) return { base: dv.getBase(), target: dv.getTarget() };
  const pr = store.get('pr');
  if (pr?.headSha) return { base: pr.mergeBaseSha || pr.baseSha || 'HEAD', target: pr.headSha };
  return { base: 'HEAD', target: 'worktree' };
}

/** Resolves with a finished job's result, or rejects with its error. */
function waitJob(id, onProgress) {
  return new Promise((resolve, reject) => {
    const settle = (j) => {
      if (j?.id !== id) return;
      if (j.state === 'done' || j.state === 'failed' || j.state === 'cancelled') {
        off();
        clearInterval(poll);
        if (j.state === 'done') resolve(j.result);
        else reject(new Error(j.error?.message || 'The check failed.'));
      } else onProgress(j.progress);
    };
    const off = bus.on('ev:job', settle);
    // Events can be missed across a reconnect: poll while it runs.
    const poll = setInterval(() => request(`jobs/${encodeURIComponent(id)}`).then(settle).catch(() => {}), 2000);
  });
}

const draftKey = () => `ferro.intent.${store.get('meta')?.workspace?.key || ''}`;
function readDraft() {
  try { return localStorage.getItem(draftKey()) || ''; } catch { return ''; }
}
function saveDraft(text) {
  try { localStorage.setItem(draftKey(), text); } catch { /* private mode: the text just isn't kept */ }
}

export function createIntentPanel(ctx) {
  const text = h('textarea', {
    class: 'input intent-text',
    rows: '5',
    placeholder: 'e.g. Limit password reset requests to 5 an hour per account, and expire reset links after 30 minutes.',
  });
  text.value = readDraft();
  text.addEventListener('input', () => saveDraft(text.value));
  let pair = currentPair(ctx);
  let baseEdited = false;
  const base = h('input', { class: 'input sm mono intent-base', 'aria-label': 'Compare with', value: pair.base, spellcheck: 'false' });
  base.addEventListener('input', () => { baseEdited = true; });
  const runBtn = h('button', { class: 'btn sm primary', on: { click: () => run() } }, icon('check-circle', 'xs'), 'Check');
  const status = h('p', { class: 'faint small', role: 'status' });
  const out = h('div', { class: 'intent-out' });
  const el = h('div', { class: 'intent', hidden: true },
    h('label', { class: 'col intent-ask' }, h('span', { class: 'f-title' }, 'What should this change do?'), text),
    h('div', { class: 'row intent-bar' }, h('label', { class: 'row faint small' }, 'Compare with', base), h('span', { class: 'grow' }), runBtn),
    status, out);
  text.addEventListener('keydown', (e) => { if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) { e.preventDefault(); run(); } });

  const open = (path, line) => ctx.onOpen?.(path, { focus: true, line: line || undefined, preview: false });

  async function run() {
    const intent = text.value.trim();
    if (!intent) { status.textContent = 'Describe what the change should do first.'; text.focus(); return; }
    pair = currentPair(ctx);
    runBtn.disabled = true;
    status.textContent = STAGES.diff;
    mount(out, []);
    try {
      const { job } = await request('ai/intent', { method: 'POST', body: { intent, base: base.value.trim() || pair.base, target: pair.target } });
      const r = await waitJob(job.id, (p) => { status.textContent = STAGES[p?.stage] || STAGES.ai; });
      status.textContent = '';
      mount(out, view(r, open));
    } catch (e) {
      status.textContent = '';
      mount(out, h('p', { class: 'agent-error' }, e.message));
    } finally {
      runBtn.disabled = false;
    }
  }

  return {
    el,
    focus() {
      // Follow what is on screen until the person picks a base themselves.
      if (!baseEdited) base.value = currentPair(ctx).base;
      text.focus();
    },
  };
}

function view(r, open) {
  const reqs = r.requirements || [];
  // The file name fits the narrow panel; the full path is the tooltip and the accessible name.
  const at = (x) => {
    if (!x.path) return null;
    const line = x.line ? `:${x.line}` : '';
    return h('button', { class: 'link mono small', 'aria-label': `${x.path}${line}`, 'data-tip': x.path, on: { click: () => open(x.path, x.line) } }, `${basename(x.path)}${line}`);
  };
  const section = (title, list, row) => (list?.length ? [h('div', { class: 'intent-label' }, title), h('ul', { class: 'intent-list' }, list.map(row))] : null);
  const copy = () => {
    navigator.clipboard?.writeText(r.markdown || '');
    toast({ kind: 'ok', title: 'Copied as Markdown', timeout: 1800 });
  };
  return [
    h('div', { class: `intent-verdict ${r.verdict}` },
      h('strong', null, VERDICT[r.verdict] || r.verdict),
      h('span', { class: 'faint small' }, `${r.counts?.done || 0} of ${reqs.length} done`)),
    r.summary ? h('p', { class: 'intent-summary' }, r.summary) : null,
    h('ul', { class: 'intent-list' }, reqs.map((q) => h('li', { class: 'intent-req' },
      h('span', { class: `intent-status ${q.status}` }, STATUS[q.status] || q.status),
      h('div', { class: 'grow' },
        h('div', null, q.text),
        q.note ? h('div', { class: 'faint small' }, q.note) : null,
        q.evidence?.length ? h('div', { class: 'row intent-at' }, q.evidence.map(at)) : null)))),
    section('Changes your description does not mention', r.unrequested, (u) => h('li', null, at(u), ' ', u.what)),
    section('Edge cases not handled', r.edgeCases, (e) => h('li', null, e.text, e.path ? [' ', at(e)] : null)),
    section('Tests to add', r.tests, (t) => h('li', null, t.text, t.path ? h('span', { class: 'faint mono small' }, ` ${t.path}`) : null)),
    h('div', { class: 'row intent-foot' },
      h('button', { class: 'btn sm', on: { click: copy } }, icon('copy', 'xs'), 'Copy as Markdown'),
      h('span', { class: 'faint small' }, `${r.model || ''}${r.cached ? ' · cached' : ''}`)),
  ].flat(Infinity).filter(Boolean);
}
