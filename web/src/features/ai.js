// AI feature (F4, FRONTEND.md § 6.14): the ask panel, AI review UX, and the ✦ commit message
// button's backing call. Streaming answers render through the escape-first mini renderer
// (core/minimd.js) while tokens arrive, then swap once for backend-rendered markdown on the
// `final` event (§ 4 rule 7). Findings from /ai/review arrive as `job` events on the existing
// SSE stream and are pushed into both this panel and the open diff view's gutter.
import { h, mount, setTrustedHTML } from '../core/dom.js';
import { api, has, isAbort } from '../core/api.js';
import { aiApi } from '../core/ai-api.js';
import { store } from '../core/store.js';
import { bus } from '../core/bus.js';
import { renderMiniMarkdown, createAnswerStream } from '../core/minimd.js';
import { formatCount, formatMs, plural, basename } from '../core/util.js';
import { icon } from '../ui/icons.js';
import { toast } from '../ui/overlay.js';
import { openDialog } from '../ui/dialog.js';
import { createIntentPanel } from './intent.js';

const SEV_ORDER = ['nit', 'low', 'medium', 'high'];
const SEV_LABEL = { high: 'High', medium: 'Medium', low: 'Low', nit: 'Nit' };
const CATEGORY_LABEL = { bug: 'Bug', security: 'Security', performance: 'Performance', tests: 'Tests', maintainability: 'Maintainability', style: 'Style' };
const FOCUS_OPTIONS = [['bugs', 'Bugs'], ['security', 'Security'], ['performance', 'Performance'], ['tests', 'Tests'], ['maintainability', 'Maintainability']];

// ---------- entry point ----------
export function renderAiTab(el, ctx = {}) {
  const seg = h('div', { class: 'seg ai-seg', role: 'group', 'aria-label': 'AI mode', hidden: true },
    h('button', { 'aria-pressed': 'true', 'data-mode': 'ask', on: { click: () => setMode('ask') } }, 'Ask'),
    h('button', { 'aria-pressed': 'false', 'data-mode': 'review', on: { click: () => setMode('review') } }, 'Review'),
    has('ai.intent') ? h('button', { 'aria-pressed': 'false', 'data-mode': 'intent', 'data-tip': 'Does the change do what it should?', on: { click: () => setMode('intent') } }, 'Intent') : null);
  const body = h('div', { class: 'ai-body' });
  mount(el, h('div', { class: 'ai-head' }, seg), body);

  let mode = 'ask';
  let panels = null;
  // Other surfaces (the PR bar, the palette) ask for a mode through the store: this tab renders
  // on first show, after they have already asked.
  store.subscribe('aiMode', (m) => { if (m && panels) setMode(m); });

  function setMode(next) {
    mode = next;
    for (const b of seg.children) b.setAttribute('aria-pressed', String(b.dataset.mode === mode));
    if (panels) {
      panels.ask.el.hidden = mode !== 'ask';
      panels.review.el.hidden = mode !== 'review';
      if (panels.intent) panels.intent.el.hidden = mode !== 'intent';
      if (mode === 'ask') panels.ask.focus();
      if (mode === 'intent') panels.intent?.focus();
    }
  }

  (async () => {
    if (!has('ai')) { renderSetupCard(body, null); return; }
    let status = null;
    try { status = await aiApi.status(); } catch { /* offline: treat like unconfigured */ }
    if (!status?.configured) { renderSetupCard(body, status); return; }
    const ask = createAskPanel(ctx);
    const review = createReviewPanel(ctx, { onAskFollowup: (f) => { setMode('ask'); ask.prefillFromFinding(f); } });
    const intent = has('ai.intent') ? createIntentPanel(ctx) : null;
    panels = { ask, review, intent };
    mount(body, ask.el, review.el, intent?.el);
    seg.hidden = false;
    setMode(store.get('aiMode') || 'ask');
  })();

  return { show: () => panels?.ask.focus() };
}

function renderSetupCard(el, status) {
  mount(el, h('div', { class: 'ai-intro' },
    h('span', { class: 'a-icon' }, icon('sparkles', 'lg')),
    h('h3', null, status ? 'AI needs a provider' : 'AI is unavailable'),
    h('p', { class: 'muted' }, status
      ? 'No provider key was found where ferro runs. Set one and reopen this panel.'
      : 'This server does not report the AI capability.'),
    status?.providers?.length
      ? h('ul', { class: 'ai-list' }, status.providers.map((p) => h('li', null, icon(p.configured ? 'check' : 'circle', 'sm'), `${p.label}${p.configured ? ' — configured' : ''}`)))
      : null,
    h('p', { class: 'faint small' }, 'Provider keys come from an environment variable (e.g. ANTHROPIC_API_KEY) or the OS keychain — ferro never asks for one in the browser.')));
}

// ---------- ask panel ----------
function createAskPanel({ editor, onOpen, peekDiffView } = {}) {
  const list = h('div', { class: 'ask-list', role: 'log', 'aria-label': 'Conversation', 'aria-live': 'polite' });
  const empty = h('div', { class: 'empty ask-empty' }, icon('sparkles', 'xl'), h('h3', null, 'Ask about this code'), h('p', null, 'Questions can use the current file, selection, or diff as context.'));
  const chipsRow = h('div', { class: 'ask-chips' });
  const textarea = h('textarea', {
    class: 'ask-input', rows: '2', placeholder: 'Ask a question… (Enter to send, Shift+Enter for a new line)', 'aria-label': 'Ask AI',
    on: { input: autoGrow, keydown: (e) => { if (e.key === 'Enter' && !e.shiftKey) { e.preventDefault(); send(); } } },
  });
  const stopBtn = h('button', { class: 'btn sm ask-stop-btn', type: 'button', hidden: true, on: { click: () => cancel() } }, icon('stop', 'xs'), 'Stop');
  const sendBtn = h('button', { class: 'btn sm primary ask-send-btn', type: 'button', on: { click: () => send() } }, icon('send', 'xs'), 'Ask');
  const composer = h('div', { class: 'ask-composer' }, chipsRow, h('div', { class: 'ask-input-row' }, textarea, stopBtn, sendBtn));
  const el = h('div', { class: 'ask-panel' }, list, composer);
  mount(list, empty);

  let conversationId = null;
  let controller = null;
  let sending = false;
  let cancelled = false;
  let gotFinal = false;
  let pendingContext = null;
  const enabledChips = new Set(['file', 'selection', 'diff']);

  function autoGrow() {
    textarea.style.height = 'auto';
    textarea.style.height = `${Math.min(160, textarea.scrollHeight)}px`;
  }

  function chipCandidates() {
    const out = [];
    const path = editor?.active;
    if (path) out.push({ id: 'file', kind: 'file', label: basename(path), ic: 'file-text' });
    const cur = store.get('cursor');
    if (cur?.path === path && cur.selStart != null && cur.selEnd != null && cur.selStart !== cur.selEnd) {
      out.push({ id: 'selection', kind: 'selection', label: `Lines ${Math.min(cur.selStart, cur.selEnd)}–${Math.max(cur.selStart, cur.selEnd)}`, ic: 'hash' });
    }
    // The open diff (never forces the lazy diff view to load just to read context).
    const dv = peekDiffView?.();
    const dc = dv && !dv.el.hidden ? { path: dv.getActivePath(), base: dv.getBase() } : null;
    if (dc?.path) out.push({ id: 'diff', kind: 'diff', label: `Diff · ${basename(dc.path)}`, ic: 'git-compare', base: dc.base, path: dc.path });
    return out;
  }

  function renderChips() {
    mount(chipsRow, chipCandidates().map((c) => h('button', {
      class: `chip ask-chip${enabledChips.has(c.id) ? ' on' : ''}`,
      type: 'button',
      'aria-pressed': String(enabledChips.has(c.id)),
      on: { click: () => { if (enabledChips.has(c.id)) enabledChips.delete(c.id); else enabledChips.add(c.id); renderChips(); } },
    }, icon(c.ic, 'xs'), c.label)));
  }
  store.subscribe('active', renderChips);
  store.subscribe('cursor', renderChips);
  renderChips();

  function buildContext() {
    const cands = chipCandidates().filter((c) => enabledChips.has(c.id));
    if (!cands.length) return undefined;
    const out = {};
    for (const c of cands) {
      if (c.kind === 'file') out.path = editor.active;
      else if (c.kind === 'selection') {
        const cur = store.get('cursor');
        out.path = editor.active;
        out.startLine = Math.min(cur.selStart, cur.selEnd);
        out.endLine = Math.max(cur.selStart, cur.selEnd);
      } else if (c.kind === 'diff') {
        out.diff = { base: c.base, path: c.path };
      }
    }
    return out;
  }

  function scrollToBottom() { list.scrollTop = list.scrollHeight; }

  function prefillFromFinding(finding) {
    textarea.value = `About "${finding.title}" at ${finding.path}:${finding.line} — `;
    autoGrow();
    textarea.focus();
    textarea.selectionStart = textarea.selectionEnd = textarea.value.length;
    pendingContext = { path: finding.path, startLine: finding.line, endLine: finding.line };
  }

  function cancel() {
    if (!sending) return;
    cancelled = true;
    controller?.abort();
  }

  async function send() {
    const text = textarea.value.trim();
    if (!text || sending) return;
    const context = pendingContext || buildContext();
    pendingContext = null;
    textarea.value = '';
    autoGrow();
    empty.remove();
    sending = true;
    cancelled = false;
    gotFinal = false;
    sendBtn.hidden = true;
    stopBtn.hidden = false;
    textarea.disabled = true;

    list.appendChild(h('div', { class: 'ask-turn user' }, h('div', { class: 'ask-turn-text' }, text)));

    const toolsEl = h('div', { class: 'ask-tools' });
    const answerEl = h('div', { class: 'ask-answer' });
    const statusEl = h('span', { class: 'ask-status' }, h('span', { class: 'spinner' }), ' Thinking…');
    const citesEl = h('div', { class: 'ask-cites' });
    const usageEl = h('div', { class: 'ask-usage' });
    const assistantTurn = h('div', { class: 'ask-turn assistant' }, toolsEl, answerEl, statusEl, citesEl, usageEl);
    list.appendChild(assistantTurn);
    scrollToBottom();

    const stream = createAnswerStream();
    const toolEls = new Map();
    controller = new AbortController();

    try {
      await aiApi.ask({ question: text, conversationId, context }, {
        signal: controller.signal,
        onEvent(event, data) {
          switch (event) {
            case 'meta':
              conversationId = data?.conversationId || conversationId;
              break;
            case 'token': {
              const buf = stream.token(data?.text || '');
              if (buf != null) renderMiniMarkdown(answerEl, buf);
              scrollToBottom();
              break;
            }
            case 'tool_start': {
              const row = h('details', { class: 'ask-tool' }, h('summary', null, h('span', { class: 'spinner' }), ` ${data.name}`));
              toolEls.set(data.id, { row, startedAt: performance.now() });
              toolsEl.appendChild(row);
              scrollToBottom();
              break;
            }
            case 'tool_result': {
              const t = toolEls.get(data.id);
              if (!t) break;
              const ms = data.ms ?? Math.round(performance.now() - t.startedAt);
              mount(t.row,
                h('summary', null, icon(data.ok ? 'check' : 'alert', 'xs'), ` ${data.name} · ${formatMs(ms)}`),
                h('pre', { class: 'ask-tool-out' }, (data.output || '') + (data.truncated ? '\n… (truncated)' : '')));
              break;
            }
            case 'final': {
              gotFinal = true;
              statusEl.remove();
              renderCitations(citesEl, data?.citations, onOpen);
              aiApi.renderMarkdown(data?.text || '').then((doc) => {
                const html = stream.final(doc.html);
                if (html !== false) setTrustedHTML(answerEl, html, 'markdown');
              }).catch(() => { stream.final(''); });
              break;
            }
            case 'usage':
              renderUsage(usageEl, data);
              break;
            case 'error':
              statusEl.remove();
              mount(answerEl, h('div', { class: 'ask-error' }, icon('alert', 'xs'), data?.message || 'The request failed.'));
              break;
            default:
              break;
          }
        },
      });
    } catch (e) {
      if (!isAbort(e)) {
        statusEl.remove();
        mount(answerEl, h('div', { class: 'ask-error' }, icon('alert', 'xs'), e.message));
      }
    } finally {
      if (cancelled && !gotFinal) {
        statusEl.remove();
        assistantTurn.appendChild(h('div', { class: 'ask-cancelled' }, icon('stop', 'xs'), ' Cancelled'));
      }
      sending = false;
      cancelled = false;
      controller = null;
      sendBtn.hidden = false;
      stopBtn.hidden = true;
      textarea.disabled = false;
      textarea.focus();
      scrollToBottom();
    }
  }

  return { el, focus: () => textarea.focus(), prefillFromFinding };
}

async function renderCitations(container, citations, onOpen) {
  mount(container);
  if (!citations?.length) return;
  let resolved = {};
  try {
    const candidates = citations.map((c) => `${c.path}${c.line ? `:${c.line}` : ''}`);
    resolved = (await api.resolve(candidates)).resolved || {};
  } catch { /* fall back to the raw citation */ }
  mount(container, citations.map((c) => {
    const key = `${c.path}${c.line ? `:${c.line}` : ''}`;
    const hit = resolved[key];
    const path = hit?.path || c.path;
    const line = hit?.line ?? c.line;
    const chip = h('button', { class: 'chip cite-chip', type: 'button' }, icon('file-text', 'xs'), `${basename(path)}${line ? `:${line}` : ''}`);
    chip.addEventListener('click', () => onOpen?.(path, { line, focus: true }));
    return chip;
  }));
}

function renderUsage(el, u) {
  if (!u) { mount(el); return; }
  const parts = [`${formatCount(u.inputTokens || 0)} in`, `${formatCount(u.outputTokens || 0)} out`];
  if (u.cacheReadTokens) parts.push(`${formatCount(u.cacheReadTokens)} cache read`);
  if (u.cacheWriteTokens) parts.push(`${formatCount(u.cacheWriteTokens)} cache write`);
  mount(el, h('span', { class: 'ask-usage-text' }, parts.join(' · ')));
}

// ---------- AI review ----------
function createReviewPanel({ onOpen, getDiffView } = {}, handlers = {}) {
  const startBtn = h('button', { class: 'btn sm primary', type: 'button', on: { click: openReviewDialog } }, icon('sparkles', 'xs'), 'Review changes…');
  const summaryEl = h('span', { class: 'review-summary muted small' });
  const bulkBtn = h('button', { class: 'btn xs review-bulk-btn', type: 'button', hidden: true, on: { click: acceptAllHigh } }, 'Accept all high');
  const head = h('div', { class: 'review-head' }, startBtn, summaryEl, bulkBtn);

  const sevSelect = h('select', { class: 'review-filter', 'aria-label': 'Minimum severity', on: { change: onFilter } },
    SEV_ORDER.map((s) => h('option', { value: s, selected: s === 'medium' ? true : undefined }, SEV_LABEL[s])));
  const confInput = h('input', { class: 'input sm review-filter num', type: 'number', min: '0', max: '1', step: '0.05', value: '0.5', 'aria-label': 'Minimum confidence', on: { input: onFilter } });
  const catSelect = h('select', { class: 'review-filter', 'aria-label': 'Category', on: { change: onFilter } },
    h('option', { value: 'all' }, 'All categories'),
    Object.entries(CATEGORY_LABEL).map(([k, v]) => h('option', { value: k }, v)));
  const filtersBar = h('div', { class: 'review-filters' },
    h('label', null, 'Severity ≥ ', sevSelect),
    h('label', null, 'Confidence ≥ ', confInput),
    h('label', null, 'Category ', catSelect));

  const outdatedBanner = h('div', { class: 'review-outdated', hidden: true }, icon('alert', 'xs'), ' HEAD changed since this review — findings may be outdated.');
  const listEl = h('div', { class: 'review-list' });
  const empty = h('div', { class: 'empty review-empty' }, icon('git-pull-request', 'xl'), h('h3', null, 'No review yet'), h('p', null, 'Review the current changes for bugs, security issues, performance, tests, and maintainability.'));
  // Team review memory held these back (API.md § 17): listed so nothing disappears silently.
  const suppressedEl = h('details', { class: 'review-suppressed', hidden: true });
  function renderSuppressed(list) {
    suppressedEl.hidden = !list.length;
    mount(suppressedEl,
      h('summary', { class: 'faint small' }, `${plural(list.length, 'finding')} hidden by team memory`),
      h('ul', { class: 'small' }, list.map((f) => h('li', null, `${f.title} · ${basename(f.path)}:${f.line}`, f.suppressedBy?.reason ? h('span', { class: 'faint' }, ` (${f.suppressedBy.reason})`) : null))),
      h('button', { class: 'link-btn small', on: { click: () => bus.emit('memory:open') } }, 'Open team memory'));
  }
  const el = h('div', { class: 'review-panel', hidden: true }, head, filtersBar, outdatedBanner, suppressedEl, listEl);
  mount(listEl, empty);

  let findings = [];
  let streamed = 0;
  let jobId = null;
  let reviewedHeadSha = null;
  let reviewPair = null; // "base..target" of the change under review (Checks → Every angle)
  let diffViewRef = null;
  let closePopover = null;

  function openReviewDialog() {
    const focusRow = FOCUS_OPTIONS.map(([id, label]) => h('label', { class: 'review-focus-opt' }, h('input', { type: 'checkbox', value: id }), label));
    const baseInput = h('input', { class: 'input sm', value: diffViewRef?.getBase?.() || 'HEAD', 'aria-label': 'Base reference' });
    const prMode = store.get('meta')?.mode === 'pr';
    const scopeSelect = h('select', { class: 'review-filter', 'aria-label': 'Scope' },
      prMode ? h('option', { value: 'pr', selected: true }, 'Pull request') : null,
      h('option', { value: 'changes' }, 'Current changes'));
    openDialog({
      title: 'Review changes with AI',
      body: h('div', { class: 'col', style: { gap: '12px' } },
        h('label', null, 'Scope ', scopeSelect),
        h('label', null, 'Base', baseInput),
        h('div', { class: 'review-focus-grid' }, focusRow)),
      actions: [
        { label: 'Cancel' },
        {
          label: 'Review',
          primary: true,
          run: () => startReview({
            scope: scopeSelect.value,
            base: baseInput.value.trim() || 'HEAD',
            focus: focusRow.filter((l) => l.firstChild.checked).map((l) => l.firstChild.value),
          }),
        },
      ],
    });
  }

  // Another feature asked for a review (Checks → AI security review): start it once.
  store.subscribe('aiReviewRequest', (req) => {
    if (!req) return;
    store.set('aiReviewRequest', null);
    startReview({ scope: store.get('meta')?.mode === 'pr' ? 'pr' : 'changes', base: req.base || 'HEAD', focus: req.focus });
  }, { now: true });

  async function startReview({ scope, base, focus }) {
    findings = [];
    streamed = 0;
    outdatedBanner.hidden = true;
    reviewedHeadSha = store.get('git')?.headSha;
    const pr = store.get('pr');
    reviewPair = scope === 'pr' && pr ? `${pr.mergeBaseSha || pr.baseSha || 'HEAD'}..${pr.headSha}` : `${base}..worktree`;
    startBtn.disabled = true;
    summaryEl.textContent = 'Reviewing…';
    renderList();
    try {
      const { job } = await aiApi.review({ scope, base: scope === 'pr' ? undefined : base, focus: focus?.length ? focus : undefined });
      jobId = job.id;
    } catch (e) {
      startBtn.disabled = false;
      summaryEl.textContent = '';
      toast({ kind: 'error', title: 'Could not start the review', message: e.message });
    }
  }

  bus.on('ev:job', (data) => {
    if (!data || data.id !== jobId) return;
    if (data.progress?.finding) {
      // B5 streams a partial { path, line, title } per finding; the full Finding (id, side,
      // severity, …) only arrives in the final result. Count partials, render full ones.
      const f = data.progress.finding;
      if (f.id && f.severity) {
        if (!findings.some((x) => x.id === f.id)) { findings.push(f); renderList(); }
      } else {
        streamed += 1;
      }
      summaryEl.textContent = `Reviewing… ${plural(Math.max(streamed, findings.length), 'finding')} so far`;
      return;
    }
    if (data.state === 'done') {
      findings = data.result?.findings || findings;
      let text = data.result?.summary || '';
      const u = data.result?.usage;
      if (u) text += `${text ? ' · ' : ''}${formatCount(u.inputTokens || 0)} in / ${formatCount(u.outputTokens || 0)} out${u.cacheReadTokens ? ` / ${formatCount(u.cacheReadTokens)} cache read` : ''}`;
      summaryEl.textContent = text;
      renderSuppressed(data.result?.suppressed || []);
      startBtn.disabled = false;
      pushFindingsToDiff();
      renderList();
      publishAngles();
    } else if (data.state === 'failed' || data.state === 'cancelled') {
      startBtn.disabled = false;
      summaryEl.textContent = '';
      toast({ kind: 'error', title: 'AI review failed', message: data.error?.message || `The job was ${data.state}.` });
    }
  });

  store.subscribe('git', (g) => {
    if (reviewedHeadSha && g?.headSha && g.headSha !== reviewedHeadSha && findings.length) outdatedBanner.hidden = false;
  });

  // Checks → Every angle: the findings counted by the angle they belong to.
  const ANGLE_OF = { bug: 'correctness', security: 'security', performance: 'performance', tests: 'tests', maintainability: 'conventions', style: 'conventions' };
  function publishAngles() {
    const count = {};
    for (const id of Object.values(ANGLE_OF)) count[id] = { n: 0, high: false };
    for (const f of findings) {
      if (f.dismissed) continue;
      const c = count[ANGLE_OF[f.category]] || count.correctness;
      c.n += 1;
      c.high ||= f.severity === 'high';
    }
    store.update('angles', Object.fromEntries(Object.entries(count).map(([id, c]) => [id,
      { pair: reviewPair, state: c.high ? 'fail' : c.n ? 'warn' : 'ok', text: c.n ? plural(c.n, 'AI finding') : 'AI review: none' }])));
  }

  function pushFindingsToDiff() {
    getDiffView?.().then((v) => {
      diffViewRef = v;
      v.setFindings(findings.filter((f) => !f.dismissed && passesFilter(f)), { onOpen: (dot, finding) => openFindingPopover(dot, finding) });
    }).catch(() => {});
  }

  // The gutter shows the same findings as the list; only re-push once the diff view exists.
  function onFilter() {
    renderList();
    if (diffViewRef) pushFindingsToDiff();
  }

  function passesFilter(f) {
    if (SEV_ORDER.indexOf(f.severity) < SEV_ORDER.indexOf(sevSelect.value)) return false;
    if ((f.confidence ?? 0) < (parseFloat(confInput.value) || 0)) return false;
    if (catSelect.value !== 'all' && f.category !== catSelect.value) return false;
    return true;
  }

  function renderList() {
    if (!findings.length) { mount(listEl, empty); bulkBtn.hidden = true; return; }
    const visible = findings.filter((f) => !f.dismissed && passesFilter(f));
    bulkBtn.hidden = !visible.some((f) => f.severity === 'high' && !f.accepted);
    if (!visible.length) { mount(listEl, h('p', { class: 'faint small' }, 'No findings match these filters.')); return; }
    const byFile = new Map();
    for (const f of visible) { if (!byFile.has(f.path)) byFile.set(f.path, []); byFile.get(f.path).push(f); }
    mount(listEl, [...byFile].map(([path, fs]) => h('div', { class: 'review-file-group' },
      h('div', { class: 'review-file-head' }, icon('file-text', 'xs'), h('span', { class: 'truncate' }, path), h('span', { class: 'count num' }, String(fs.length))),
      fs.slice().sort((a, b) => a.line - b.line).map(renderCard))));
  }

  function renderCard(f) {
    const bodyEl = h('div', { class: 'finding-body' });
    setTrustedHTML(bodyEl, f.bodyHtml || '', 'markdown');
    const editBox = h('div', { class: 'finding-edit', hidden: true });
    const acceptBtn = h('button', { class: 'btn xs', type: 'button', on: { click: () => acceptFinding(f) } }, 'Accept');
    const editBtn = h('button', { class: 'btn xs', type: 'button', on: { click: () => openEdit(f, editBox) } }, 'Edit & accept');
    const dismissBtn = h('button', { class: 'btn xs', type: 'button', on: { click: () => dismissFinding(f) } }, 'Dismiss');
    const followBtn = h('button', { class: 'btn xs', type: 'button', on: { click: () => handlers.onAskFollowup?.(f) } }, 'Ask follow-up');
    return h('div', { class: `finding-card sev-${f.severity}${f.accepted ? ' accepted' : ''}` },
      h('div', { class: 'finding-head' },
        h('span', { class: `sev-chip sev-${f.severity}` }, SEV_LABEL[f.severity]),
        h('span', { class: 'finding-cat' }, CATEGORY_LABEL[f.category] || f.category),
        h('span', { class: 'finding-conf num' }, `${Math.round((f.confidence || 0) * 100)}%`),
        h('button', { class: 'finding-loc num link', type: 'button', on: { click: () => onOpen?.(f.path, { line: f.line, focus: true }) } }, `${basename(f.path)}:${f.line}`)),
      h('div', { class: 'finding-title' }, f.title),
      bodyEl,
      f.suggestion ? h('pre', { class: 'finding-suggestion' }, f.suggestion) : null,
      f.accepted
        ? h('div', { class: 'finding-state ok' }, icon('check', 'xs'), ' Accepted')
        : h('div', { class: 'finding-actions' }, acceptBtn, editBtn, dismissBtn, followBtn),
      editBox);
  }

  async function acceptFinding(f, body) {
    try {
      await aiApi.acceptFinding(f.id, body ? { body } : {});
      f.accepted = true;
      closePopover?.();
      renderList();
      pushFindingsToDiff();
      toast({ kind: 'ok', title: 'Accepted', message: f.title, timeout: 1500 });
    } catch (e) {
      toast({ kind: 'error', title: 'Could not accept finding', message: e.message });
    }
  }

  function openEdit(f, editBox) {
    const editArea = h('textarea', { class: 'input', rows: '3' }, f.suggestion || f.body);
    mount(editBox,
      editArea,
      h('div', { class: 'row' },
        h('button', { class: 'btn xs primary', type: 'button', on: { click: () => { acceptFinding(f, editArea.value); editBox.hidden = true; } } }, 'Save & accept'),
        h('button', { class: 'btn xs', type: 'button', on: { click: () => { editBox.hidden = true; } } }, 'Cancel')));
    editBox.hidden = false;
  }

  function dismissFinding(f) {
    const reasonInput = h('textarea', { class: 'input', rows: '2', placeholder: 'Optional reason' });
    const remember = h('input', { type: 'checkbox' });
    openDialog({
      title: 'Dismiss finding',
      body: h('div', { class: 'col', style: { gap: '8px' } }, h('p', { class: 'muted small' }, f.title), reasonInput,
        has('memory') ? h('label', { class: 'mem-opt' }, remember, h('span', null, 'Don’t report this kind of finding again…')) : null),
      actions: [
        { label: 'Cancel' },
        {
          label: 'Dismiss',
          primary: true,
          run: async () => {
            try {
              await aiApi.dismissFinding(f.id, reasonInput.value.trim() ? { reason: reasonInput.value.trim() } : {});
              f.dismissed = true;
              closePopover?.();
              renderList();
              pushFindingsToDiff();
              if (remember.checked) {
                const m = await import('./memory.js');
                // After this dialog closes (it returns focus to where it came from).
                setTimeout(() => m.openRuleDialog({ appliesTo: 'ai', category: f.category, title: f.title, path: f.path }), 0);
              }
            } catch (e) {
              toast({ kind: 'error', title: 'Could not dismiss finding', message: e.message });
              return false;
            }
            return true;
          },
        },
      ],
    });
  }

  async function acceptAllHigh() {
    const targets = findings.filter((f) => f.severity === 'high' && !f.accepted && !f.dismissed && passesFilter(f));
    if (!targets.length) return;
    bulkBtn.disabled = true;
    await Promise.allSettled(targets.map((f) => aiApi.acceptFinding(f.id).then(() => { f.accepted = true; })));
    bulkBtn.disabled = false;
    renderList();
    pushFindingsToDiff();
    toast({ kind: 'ok', title: `Accepted ${plural(targets.length, 'finding')}` });
  }

  function openFindingPopover(anchorEl, finding) {
    closePopover?.();
    const card = renderCard(finding);
    card.classList.add('finding-popover');
    const closeBtn = h('button', { class: 'icon-btn xs finding-popover-close', 'aria-label': 'Close' });
    mount(closeBtn, icon('x', 'xs'));
    card.insertBefore(closeBtn, card.firstChild);
    document.body.appendChild(card);
    const r = anchorEl.getBoundingClientRect();
    card.style.position = 'fixed';
    card.style.left = `${Math.max(6, Math.min(r.left, window.innerWidth - 360))}px`;
    card.style.top = `${Math.min(r.bottom + 6, window.innerHeight - 220)}px`;
    card.style.zIndex = '80';
    function onDocDown(e) { if (!card.contains(e.target) && e.target !== anchorEl) cleanup(); }
    function onKey(e) { if (e.key === 'Escape') cleanup(); }
    function cleanup() {
      card.remove();
      document.removeEventListener('pointerdown', onDocDown, true);
      document.removeEventListener('keydown', onKey, true);
      closePopover = null;
    }
    document.addEventListener('pointerdown', onDocDown, true);
    document.addEventListener('keydown', onKey, true);
    closeBtn.addEventListener('click', cleanup);
    closePopover = cleanup;
  }

  return { el };
}
