// Review mode: PR bar, review overview, PR file list, inline threads, composer, submit dialog.
// Follows FRONTEND.md § 6.13, § 4 (Security: NO HTML sink for comment bodies), and API.md § 8–9.
import { h, mount, append, setTrustedHTML } from '../core/dom.js';
import { request, has } from '../core/api.js';
import { store } from '../core/store.js';
import { bus } from '../core/bus.js';
import { execute } from '../core/commands.js';
import { plural } from '../core/util.js';
import { icon, fileIcon } from '../ui/icons.js';
import { toast } from '../ui/overlay.js';
import { openDialog } from '../ui/dialog.js';

// Review-mode transport (kept out of core/api.js's eager surface — see the note there).
const api = {
  refreshPr: () => request('pr/refresh', { method: 'POST', body: {} }),
  threads: (o) => request('pr/threads', o),
  replyThread: (id, body) => request(`pr/threads/${encodeURIComponent(id)}/reply`, { method: 'POST', body: { body } }),
  prConversation: (body) => request('pr/conversation', { method: 'POST', body: { body } }),
  drafts: (o) => request('review/drafts', o),
  createDraft: (draft) => request('review/drafts', { method: 'POST', body: draft }),
  patchDraft: (id, patch) => request(`review/drafts/${encodeURIComponent(id)}`, { method: 'PATCH', body: patch }),
  deleteDraft: (id) => request(`review/drafts/${encodeURIComponent(id)}`, { method: 'DELETE' }),
  submitReview: ({ event, body }) => request('review/submit', { method: 'POST', body: { event, body } }),
  viewed: (o) => request('review/viewed', o),
  putViewed: (path, viewed) => request('review/viewed', { method: 'PUT', body: { path, viewed } }),
  rounds: (o) => request('review/rounds', o),
  renderMarkdown: (text, context) => request('markdown/render', { method: 'POST', body: { text, path: context?.path, context } }),
};

let activeComp = null;
let drafts = [];
let threads = [];
let conversation = [];
let rounds = [];
let viewedState = {};
let curRound = null;

export const getActiveComposer = () => activeComp;
export const setActiveComposer = (c) => { activeComp = c; bus.emit('review:composer:changed', c); };
export const getDrafts = () => drafts;
export const getThreads = () => threads;
export const getViewedState = () => viewedState;
export const getRounds = () => rounds;

export async function fetchReviewState() {
  const pr = store.get('pr');
  if (!pr && !has('review.drafts')) return;
  try {
    const [d, t, v, r] = await Promise.all([
      api.drafts().catch(() => ({ drafts: [] })),
      pr ? api.threads().catch(() => ({ threads: [], conversation: [] })) : { threads: [], conversation: [] },
      pr ? api.viewed().catch(() => ({ files: {} })) : { files: {} },
      pr ? api.rounds().catch(() => ({ rounds: [] })) : { rounds: [] },
    ]);
    drafts = d.drafts || [];
    threads = t.threads || [];
    conversation = t.conversation || [];
    viewedState = v.files || {};
    rounds = r.rounds || [];
    bus.emit('review:state:updated');
  } catch (e) {
    console.warn('[review] state fetch failed', e);
  }
}

bus.on('ev:drafts', (d) => {
  if (d?.drafts) { drafts = d.drafts; bus.emit('review:drafts:updated', drafts); }
  else fetchReviewState();
});
bus.on('ev:threads', () => {
  if (store.get('pr')) api.threads().then((t) => { threads = t.threads || []; conversation = t.conversation || []; bus.emit('review:threads:updated'); }).catch(() => {});
});

let installed = false;
/** First PR of the session: PR bar in the shell's prSlot, conversation tab in the inspector. */
export function installReview(shell) {
  if (installed) return;
  installed = true;
  // Top of main (§ 6.13): a full-width row under the topbar, which has no room for it.
  shell.banners.prepend(createPrBar().el);
  fetchReviewState();
  shell.registerInspectorTab({
    id: 'conversation', title: 'Conversation', icon: 'message',
    render: (el) => {
      renderConversationTab(el);
      store.subscribe('pr', () => renderConversationTab(el));
      bus.on('review:threads:updated', () => renderConversationTab(el));
      bus.on('review:state:updated', () => renderConversationTab(el));
    },
  });
}

/** Only http(s) links from PR metadata become hrefs (no `javascript:` from an untrusted host). */
const safeUrl = (u) => (/^https?:\/\//i.test(u || '') ? u : '#');

export function createPrBar() {
  const el = h('div', { class: 'pr-bar', hidden: true });
  const headMovedBanner = h('div', { class: 'pr-head-moved-banner', hidden: true });
  const left = h('div', { class: 'pr-bar-left' });
  const right = h('div', { class: 'pr-bar-right' });
  mount(el, left, right);
  const wrapper = h('div', { class: 'pr-bar-wrapper' }, headMovedBanner, el);

  function render() {
    const pr = store.get('pr');
    if (!pr) { el.hidden = true; headMovedBanner.hidden = true; return; }
    el.hidden = false;

    if (pr.headMoved) {
      headMovedBanner.hidden = false;
      const refBtn = h('button', {
        class: 'btn xs pr-refresh-btn',
        on: {
          click: async () => {
            try {
              refBtn.disabled = true;
              const next = await api.refreshPr();
              store.set('pr', next);
              await fetchReviewState();
              bus.emit('diff:refresh');
              toast({ kind: 'ok', title: 'Refreshed PR', message: 'Loaded latest commits' });
            } catch (e) { toast({ kind: 'error', title: 'Refresh failed', message: e.message }); }
            finally { refBtn.disabled = false; }
          },
        },
      }, 'Refresh');
      mount(headMovedBanner, h('span', null, 'New commits pushed'), h('div', { class: 'pr-head-moved-actions' }, refBtn));
    } else {
      headMovedBanner.hidden = true;
      mount(headMovedBanner);
    }

    const prLink = h('a', { class: 'pr-number-title', href: safeUrl(pr.url), target: '_blank', rel: 'noopener noreferrer' },
      icon('git-pull-request', 'xs'), h('span', { class: 'num' }, `#${pr.number}`), h('span', { class: 'pr-title-text' }, pr.title));
    const author = h('span', { class: 'pr-author' }, pr.author?.login || '');
    const branches = h('span', { class: 'pr-branches' }, h('span', null, pr.baseRef || 'main'), ' ← ', h('span', null, pr.headRef || 'head'));
    const state = h('span', { class: `pr-state-pill ${pr.draft ? 'draft' : pr.state || 'open'}` }, pr.draft ? 'Draft' : pr.state || 'Open');
    const chk = pr.checks?.state || 'none';
    const checks = h('span', { class: `pr-checks-badge ${chk}` }, icon(chk === 'success' ? 'check' : chk === 'failure' ? 'x' : 'alert-circle', 'xs'));
    const st = pr.stats || { files: 0, additions: 0, deletions: 0 };
    const statsEl = h('span', { class: 'pr-stats num' },
      h('span', null, `${st.files} files`), h('span', { class: 'hm-dot' }, '·'),
      h('span', { class: 'diff-add-stat' }, `+${st.additions}`), h('span', { class: 'diff-del-stat' }, `-${st.deletions}`));
    const vCount = Object.values(viewedState).filter((v) => v.viewed).length;
    const vProg = h('span', { class: 'pr-viewed-progress num' }, `${vCount}/${st.files || 1} viewed`);
    mount(left, prLink, author, branches, state, checks, statsEl, vProg);

    const rSel = h('select', {
      class: 'pr-rounds-select', 'aria-label': 'Since last review',
      on: {
        change: (e) => {
          curRound = e.target.value === 'entire' ? null : e.target.value;
          bus.emit('diff:open', { base: curRound || pr.mergeBaseSha || pr.baseSha || 'HEAD', target: pr.headSha });
        },
      },
    }, h('option', { value: 'entire', selected: !curRound }, 'Entire PR'));
    rounds.forEach((r, i) => append(rSel, [h('option', { value: r.headSha, selected: curRound === r.headSha }, `Since round ${i + 1} (${r.headSha.slice(0, 7)})`)]));

    const aiBtn = h('button', {
      class: 'btn xs pr-ai-btn',
      hidden: !has('ai.review'),
      on: { click: () => execute('ai.review') },
    }, icon('sparkles', 'xs'), ' AI review');
    // Batch agent edit: every draft goes to a new agent thread as context (API.md § 10.7).
    const agentBtn = h('button', {
      class: 'btn xs pr-agent-btn',
      hidden: !has('harness.threads') || !drafts.length,
      'data-tip': 'Ask the coding agent to address your drafts',
      on: { click: () => import('./threads.js').then((m) => m.sendDraftsToAgent(drafts)) },
    }, icon('terminal', 'xs'), ` Fix ${drafts.length} with agent`);

    const subBtn = h('button', {
      class: 'btn xs primary pr-submit-btn',
      on: { click: () => openSubmitReviewDialog() },
    }, `Submit review ▾ (${drafts.length})`);
    mount(right, rSel, aiBtn, agentBtn, subBtn);
  }

  store.subscribe('pr', render);
  bus.on('review:state:updated', render);
  bus.on('review:drafts:updated', render);
  return { el: wrapper, render };
}

export function renderReviewOverview(container, { onOpenDiff } = {}) {
  const pr = store.get('pr');
  if (!pr) return null;
  const el = h('div', { class: 'review-overview' });
  const title = h('h1', { class: 'review-overview-title' },
    icon('git-pull-request'), h('span', null, `#${pr.number} ${pr.title}`),
    h('span', { class: `pr-state-pill ${pr.draft ? 'draft' : pr.state}` }, pr.draft ? 'Draft' : pr.state));
  const meta = h('div', { class: 'home-meta' },
    h('span', { class: 'pr-author' }, `By ${pr.author?.login || 'unknown'}`), h('span', { class: 'hm-dot' }, '·'),
    h('span', null, `${pr.baseRef} ← ${pr.headRef}`), h('span', { class: 'hm-dot' }, '·'),
    h('span', null, `${pr.stats?.files || 0} files (+${pr.stats?.additions || 0} -${pr.stats?.deletions || 0})`));
  const startBtn = h('button', {
    class: 'btn primary pr-start-review-btn',
    on: { click: () => (onOpenDiff ? onOpenDiff() : bus.emit('diff:open')) },
  }, icon('git-compare', 'sm'), ' View Diff & Start Review');

  const desc = h('div', { class: 'review-overview-desc md' });
  if (pr.bodyHtml) setTrustedHTML(desc, pr.bodyHtml, 'markdown');
  else desc.textContent = 'No description provided.';

  const fileRows = Object.keys(viewedState).map((f) => {
    const v = viewedState[f] || {};
    const chk = h('input', { type: 'checkbox', checked: !!v.viewed, on: { change: (e) => toggleFileViewed(f, e.target.checked) } });
    return h('div', {
      class: 'review-overview-file-row',
      on: { click: (e) => { if (e.target !== chk) (onOpenDiff ? onOpenDiff({ path: f }) : bus.emit('diff:open', { path: f })); } },
    }, h('div', { class: 'row gap-sm' }, chk, fileIcon(f), h('span', { class: 'font-mono' }, f)),
    h('div', { class: 'row gap-sm' }, v.changedSince ? h('span', { class: 'pr-changed-since-badge' }, 'changed since viewed') : null));
  });
  const filesSec = h('div', { class: 'review-overview-files' }, h('h2', { class: 'label' }, 'Changed files'), ...fileRows);
  const thSum = h('div', { class: 'home-meta' }, icon('message', 'xs'), h('span', null, `${threads.length} threads`));
  mount(el, h('div', { class: 'review-overview-head' }, title, meta), h('div', { class: 'review-overview-actions' }, startBtn), desc, filesSec, thSum);
  mount(container, el);
  return el;
}

export function renderPrFileList(container, { onOpenDiff } = {}) {
  const pr = store.get('pr');
  if (!pr) return null;
  const el = h('div', { class: 'pr-file-list-panel' });
  let activeFilt = 'all';

  const head = h('div', { class: 'pr-file-list-header' }, h('span', { class: 'font-semibold truncate' }, `#${pr.number} files`), h('span', { class: 'num text-muted' }, `${pr.stats?.files || 0} files`));
  const chips = ['all', 'unviewed', 'commented'].map((k) => h('button', {
    class: `pr-filter-chip ${k === 'all' ? 'active' : ''}`,
    on: { click: (e) => { activeFilt = k; el.querySelectorAll('.pr-filter-chip').forEach((c) => c.classList.remove('active')); e.target.classList.add('active'); renderList(); } },
  }, k[0].toUpperCase() + k.slice(1)));
  const list = h('div', { class: 'panel-body pr-files-container' });

  function renderList() {
    mount(list);
    const files = Object.entries(viewedState);
    const thCounts = new Map();
    threads.forEach((t) => thCounts.set(t.path, (thCounts.get(t.path) || 0) + (t.comments?.length || 1)));
    const filtered = files.filter(([p, v]) => (activeFilt === 'unviewed' ? !v.viewed : activeFilt === 'commented' ? (thCounts.get(p) || 0) > 0 : true));
    if (!filtered.length) { mount(list, h('p', { class: 'home-empty' }, 'No files match filter.')); return; }
    const rows = filtered.map(([path, v]) => {
      const cCount = thCounts.get(path) || 0;
      const chk = h('input', { type: 'checkbox', checked: !!v.viewed, on: { change: (e) => toggleFileViewed(path, e.target.checked) } });
      return h('div', {
        class: 'git-file-row',
        on: { click: (e) => { if (e.target !== chk) (onOpenDiff ? onOpenDiff({ path }) : bus.emit('diff:open', { path })); } },
      },
      h('div', { class: 'git-file-left truncate' }, chk, fileIcon(path), h('span', { class: 'git-file-name truncate' }, path)),
      h('div', { class: 'git-file-right' }, v.changedSince ? h('span', { class: 'pr-changed-since-badge' }, 'changed') : null, cCount ? h('span', { class: 'pr-comment-count-badge' }, icon('message', 'xs'), String(cCount)) : null));
    });
    mount(list, ...rows);
  }
  mount(el, head, h('div', { class: 'pr-file-filters' }, ...chips), list);
  mount(container, el);
  renderList();
  const unsub = bus.on('review:state:updated', renderList);
  return { el, render: renderList, destroy: unsub };
}

export async function toggleFileViewed(path, viewed) {
  try {
    const res = await api.putViewed(path, viewed);
    viewedState[path] = res;
    bus.emit('review:state:updated');
    bus.emit('review:viewed:changed', { path, viewed });
  } catch (e) { toast({ kind: 'error', title: 'Viewed update failed', message: e.message }); }
}

export function createInlineThread(thread, { onReply } = {}) {
  const container = h('div', { class: 'diff-thread-container', 'data-thread-id': thread.id });
  const card = h('div', { class: `thread-card ${thread.outdated ? 'outdated' : ''}` });
  const header = h('div', { class: 'thread-card-header' },
    h('div', { class: 'thread-author-time' }, icon('message', 'xs'), h('span', { class: 'thread-author' }, plural(thread.comments?.length || 0, 'comment')),
      thread.resolved ? h('span', { class: 'thread-badge-outdated' }, 'Resolved') : null, thread.outdated ? h('span', { class: 'thread-badge-outdated' }, 'Outdated') : null),
    h('button', { class: 'icon-btn xs', on: { click: () => { bodyEl.hidden = !bodyEl.hidden; } } }, icon('chevron-down', 'xs')));
  const bodyEl = h('div', { class: 'thread-comments' });
  if (thread.outdated) bodyEl.hidden = true;

  const commentRows = (thread.comments || []).map((c) => {
    const b = h('div', { class: 'comment-body' });
    b.textContent = c.body || ''; // UNTRUSTED: must render as plain text
    return h('div', { class: 'thread-comment' }, h('div', { class: 'comment-head' }, h('span', { class: 'font-semibold' }, c.author?.login || 'User')), b);
  });

  const replyInput = h('textarea', { class: 'thread-reply-input', placeholder: 'Reply to thread… (Cmd+Enter)' });
  const replyBtn = h('button', {
    class: 'btn xs primary',
    on: {
      click: async () => {
        const text = replyInput.value.trim();
        if (!text) return;
        try {
          replyBtn.disabled = true;
          const res = await api.replyThread(thread.id, text);
          replyInput.value = '';
          (thread.comments = thread.comments || []).push(res);
          bus.emit('review:threads:updated');
          onReply?.(thread.id, res);
          toast({ kind: 'ok', title: 'Reply sent', timeout: 1200 });
        } catch (e) { toast({ kind: 'error', title: 'Reply failed', message: e.message }); }
        finally { replyBtn.disabled = false; }
      },
    },
  }, 'Reply');
  replyInput.onkeydown = (e) => { if ((e.metaKey || e.ctrlKey) && e.key === 'Enter') { e.preventDefault(); replyBtn.click(); } };

  mount(bodyEl, ...commentRows, h('div', { class: 'thread-reply-form' }, replyInput, h('div', { class: 'thread-reply-actions' }, replyBtn)));
  mount(card, header, bodyEl);
  mount(container, card);
  return container;
}

export function createInlineComposer({ path, line, startLine, side = 'RIGHT', originalLines = '', draftId = null, initialBody = '', onSave, onCancel } = {}) {
  const container = h('div', { class: 'diff-thread-container composer-container' });
  const card = h('div', { class: 'composer-card' });
  const lText = startLine && startLine < line ? `lines ${startLine}–${line}` : `line ${line}`;
  const head = h('div', { class: 'composer-header' }, h('span', { class: 'font-semibold' }, `${draftId ? 'Edit comment' : 'Comment'} on ${lText} (${side.toLowerCase()})`), h('button', { class: 'icon-btn xs', on: { click: cancel } }, icon('x', 'xs')));
  const ta = h('textarea', { class: 'composer-textarea', placeholder: 'Leave a comment (Markdown). Cmd+Enter saves draft.' });
  ta.value = initialBody;
  const preview = h('div', { class: 'composer-preview-pane', hidden: true });

  const sugBtn = h('button', {
    class: 'btn xs',
    on: {
      click: () => {
        const blk = `\`\`\`suggestion\n${originalLines || ''}\n\`\`\`\n`;
        const { selectionStart: s, selectionEnd: e, value: v } = ta;
        ta.value = v.slice(0, s) + blk + v.slice(e);
        ta.focus();
      },
    },
  }, icon('edit', 'xs'), ' Suggest change');

  const prevBtn = h('button', {
    class: 'btn xs',
    on: {
      click: async () => {
        if (!preview.hidden) { preview.hidden = true; ta.hidden = false; prevBtn.textContent = 'Preview'; }
        else {
          try {
            const res = await api.renderMarkdown(ta.value, { path, startLine: startLine || line, endLine: line, side });
            setTrustedHTML(preview, res?.html || '', 'markdown');
          } catch { preview.textContent = ta.value; }
          preview.hidden = false; ta.hidden = true; prevBtn.textContent = 'Write';
        }
      },
    },
  }, 'Preview');

  const saveBtn = h('button', {
    class: 'btn xs primary',
    on: {
      click: async () => {
        const body = ta.value.trim();
        if (!body) return;
        try {
          saveBtn.disabled = true;
          const saved = draftId
            ? await api.patchDraft(draftId, { body, line, startLine: startLine || line, side })
            : await api.createDraft({ path, line, startLine: startLine || line, side, body, source: 'human' });
          const idx = drafts.findIndex((d) => d.id === saved.id);
          if (idx >= 0) drafts[idx] = saved; else drafts.push(saved);
          bus.emit('review:drafts:updated', drafts);
          setActiveComposer(null);
          onSave?.(saved);
          toast({ kind: 'ok', title: draftId ? 'Draft updated' : 'Draft saved', timeout: 1200 });
        } catch (e) { toast({ kind: 'error', title: 'Save draft failed', message: e.message }); }
        finally { saveBtn.disabled = false; }
      },
    },
  }, 'Save draft');

  function cancel() {
    if (ta.value.trim() && !confirm('Discard unsaved draft?')) return;
    setActiveComposer(null);
    onCancel?.();
  }

  ta.onkeydown = (e) => {
    if ((e.metaKey || e.ctrlKey) && e.key === 'Enter') { e.preventDefault(); saveBtn.click(); }
    else if (e.key === 'Escape') { e.preventDefault(); cancel(); }
  };

  mount(card, head, ta, preview, h('div', { class: 'composer-toolbar' }, h('div', { class: 'composer-tools-left' }, sugBtn, prevBtn), h('div', { class: 'composer-actions-right' }, h('button', { class: 'btn xs', on: { click: cancel } }, 'Cancel'), saveBtn)));
  mount(container, card);
  setTimeout(() => ta.focus(), 20);
  return container;
}

export function createInlineDraft(draft, { onEdit, onDelete } = {}) {
  const container = h('div', { class: 'diff-thread-container draft-container', 'data-draft-id': draft.id });
  const card = h('div', { class: 'draft-card' });
  const head = h('div', { class: 'draft-head' },
    h('div', { class: 'row gap-sm' }, h('span', { class: 'draft-badge' }, 'Pending review'), draft.stale ? h('span', { class: 'draft-stale-badge' }, 'Stale') : null),
    h('div', { class: 'draft-actions' },
      h('button', { class: 'btn xs', on: { click: () => onEdit?.(draft) } }, 'Edit'),
      h('button', {
        class: 'btn xs danger',
        on: {
          click: async () => {
            try {
              await api.deleteDraft(draft.id);
              drafts = drafts.filter((d) => d.id !== draft.id);
              bus.emit('review:drafts:updated', drafts);
              onDelete?.(draft.id);
              toast({ kind: 'ok', title: 'Draft deleted', timeout: 1200 });
            } catch (e) { toast({ kind: 'error', title: 'Delete failed', message: e.message }); }
          },
        },
      }, 'Delete')));
  const b = h('div', { class: 'comment-body' });
  b.textContent = draft.body || ''; // UNTRUSTED
  mount(card, head, b);
  mount(container, card);
  return container;
}

export function openSubmitReviewDialog() {
  const pr = store.get('pr');
  const def = store.get('settings')?.['review.defaultEvent'] || 'COMMENT';
  const form = h('div', { class: 'submit-review-form' });
  const radios = ['COMMENT', 'APPROVE', 'REQUEST_CHANGES'].map((ev) => h('label', { class: 'submit-event-radio-label' },
    h('input', { type: 'radio', name: 'review-event', value: ev, checked: def === ev }), ev[0] + ev.slice(1).toLowerCase().replace('_', ' ')));
  const ta = h('textarea', { class: 'submit-summary-textarea', placeholder: 'Review summary (optional)…' });
  const stale = drafts.filter((d) => d.stale);
  const dList = h('div', { class: 'submit-drafts-list' },
    h('span', { class: 'font-semibold' }, `${plural(drafts.length, 'draft')} to submit:`),
    stale.length ? h('span', { class: 'text-danger font-semibold' }, `⚠ ${plural(stale.length, 'stale draft')} will be skipped`) : null,
    drafts.map((d) => h('div', { class: 'truncate text-muted' }, `• ${d.path}:${d.line} — ${d.body.slice(0, 40)}…`)));

  const canReview = pr?.auth?.canReview !== false;
  const authMsg = h('div', { class: 'submit-auth-hint', hidden: canReview },
    h('span', { class: 'font-semibold' }, pr?.auth?.hasToken ? 'This token cannot submit reviews on this repository.' : 'Sign in to submit: run `gh auth login` or set GITHUB_TOKEN.'));
  const subBtn = h('button', {
    class: 'btn primary', disabled: !canReview,
    on: {
      click: async () => {
        const ev = form.querySelector('input[name="review-event"]:checked')?.value || 'COMMENT';
        try {
          subBtn.disabled = true;
          subBtn.textContent = 'Submitting…';
          const res = await api.submitReview({ event: ev, body: ta.value.trim() });
          dialog.close();
          await fetchReviewState(); // failed drafts stay (API.md § 9.2)
          const failed = res?.failed?.length || 0;
          toast({
            kind: failed ? 'warn' : 'ok',
            title: failed ? `Review submitted · ${plural(failed, 'draft')} not posted` : 'Review submitted',
            message: failed ? res.failed.map((f) => f.error).join('; ') : 'Posted to GitHub',
            action: safeUrl(res?.url) !== '#' ? { label: 'View on GitHub', run: () => window.open(res.url, '_blank', 'noopener') } : undefined,
            timeout: 6000,
          });
        } catch (e) {
          // No token: 401 with detail.hint (how to authenticate).
          if (e.detail?.hint) { authMsg.hidden = false; authMsg.firstChild.textContent = e.detail.hint; }
          toast({ kind: 'error', title: 'Submit failed', message: e.detail?.hint || e.message });
          subBtn.disabled = false;
          subBtn.textContent = 'Submit review';
        }
      },
    },
  }, 'Submit review');

  mount(form, h('div', { class: 'submit-event-radios' }, ...radios), ta, dList, authMsg, h('div', { class: 'dialog-actions' }, h('button', { class: 'btn', on: { click: () => dialog.close() } }, 'Cancel'), subBtn));
  const dialog = openDialog({ title: 'Submit Review', body: form });
}

export function renderConversationTab(container) {
  const pr = store.get('pr');
  if (!pr) { mount(container, h('p', { class: 'home-empty' }, 'No active pull request.')); return; }
  const el = h('div', { class: 'pr-conversation-tab' });
  const desc = h('div', { class: 'thread-card' }, h('div', { class: 'thread-card-header' }, h('span', { class: 'font-semibold' }, pr.author?.login || 'Author')), h('div', { class: 'thread-comment markdown-body' }));
  if (pr.bodyHtml) setTrustedHTML(desc.children[1], pr.bodyHtml, 'markdown');
  else desc.children[1].textContent = 'No description provided.';
  const cList = h('div', { class: 'thread-comments-list' }, ...conversation.map((c) => {
    const b = h('div', { class: 'thread-comment comment-body' });
    b.textContent = c.body || '';
    return h('div', { class: 'thread-card mt-sm' }, h('div', { class: 'thread-card-header' }, h('span', { class: 'font-semibold' }, c.author?.login || 'User')), b);
  }));
  const inEl = h('textarea', { class: 'thread-reply-input', placeholder: 'Add a top-level review comment…' });
  const btn = h('button', {
    class: 'btn xs primary',
    on: {
      click: async () => {
        const text = inEl.value.trim();
        if (!text) return;
        try {
          btn.disabled = true;
          const res = await api.prConversation(text);
          inEl.value = '';
          conversation.push(res);
          renderConversationTab(container);
          toast({ kind: 'ok', title: 'Comment posted', timeout: 1200 });
        } catch (e) { toast({ kind: 'error', title: 'Post failed', message: e.message }); }
        finally { btn.disabled = false; }
      },
    },
  }, 'Comment');
  mount(el, desc, cList, h('div', { class: 'thread-reply-form mt-md' }, inEl, h('div', { class: 'thread-reply-actions' }, btn)));
  mount(container, el);
}

store.subscribe('pr', (pr) => { if (pr) fetchReviewState(); });
