// Git feature: Changes view, staging, discard, commit box, branch ahead/behind, push, pull, log.
// Follows FRONTEND.md § 6.11 and API.md § 6.
import { h, mount } from '../core/dom.js';
import { request, has, ApiError } from '../core/api.js';
import { aiApi } from '../core/ai-api.js';
import { store } from '../core/store.js';
import { bus } from '../core/bus.js';
import { basename, dirname, isMac, plural } from '../core/util.js';
import { icon, fileIcon, folderIcon } from '../ui/icons.js';
import { toast } from '../ui/overlay.js';
import { openDialog } from '../ui/dialog.js';

const GIT_LABEL = {
  M: 'Modified',
  A: 'Added',
  D: 'Deleted',
  R: 'Renamed',
  C: 'Copied',
  T: 'Type changed',
  U: 'Conflict',
  '?': 'Untracked',
};

export async function stagePaths(paths) {
  const status = await request('git/stage', { method: 'POST', body: { paths } });
  store.set('git', status);
  return status;
}

export async function unstagePaths(paths) {
  const status = await request('git/unstage', { method: 'POST', body: { paths } });
  store.set('git', status);
  return status;
}

export async function discardPaths(paths) {
  const status = await request('git/discard', { method: 'POST', body: { paths, confirm: true } });
  store.set('git', status);
  return status;
}

export async function commitChanges(message, amend = false) {
  const res = await request('git/commit', { method: 'POST', body: { message, amend } });
  store.set('git', res.status);
  return res;
}

export async function pushRepo() {
  const res = await request('git/push', { method: 'POST', body: {} });
  store.set('git', res.status);
  return res;
}

export async function pullRepo() {
  const res = await request('git/pull', { method: 'POST', body: {} });
  store.set('git', res.status);
  return res;
}

export async function fetchLog(limit = 10) {
  return request('git/log', { query: { limit } });
}

export async function renderChangesPanel(section, { onOpen } = {}) {
  // PR mode (FRONTEND.md § 6.13): the Changes panel shows the PR file list, not local git status.
  let prMode = null;
  async function syncMode() {
    const wantPr = !!store.get('pr');
    if (wantPr === prMode) return;
    prMode = wantPr;
    mount(section);
    if (wantPr) {
      const { renderPrFileList } = await import('./review.js');
      renderPrFileList(section);
    } else {
      buildGitPanel();
    }
  }
  store.subscribe('pr', syncMode);
  await syncMode();
  return { refresh: () => bus.emit('git:refresh') };

  function buildGitPanel() {
  let currentBase = 'HEAD';
  let stderrText = '';

  const branchEl = h('span', { class: 'truncate git-branch-name' }, '…');
  const syncSub = h('span', { class: 'panel-sub num git-ahead-behind' }, '');
  const refreshBtn = h('button', {
    class: 'icon-btn sm',
    'aria-label': 'Refresh git status',
    'data-tip': 'Refresh',
    on: { click: () => bus.emit('git:refresh') },
  }, icon('refresh', 'sm'));

  const pushBtn = h('button', {
    class: 'icon-btn sm',
    'aria-label': 'Push commits',
    'data-tip': 'Push',
    on: { click: handlePush },
  }, icon('arrow-up', 'sm'));

  const pullBtn = h('button', {
    class: 'icon-btn sm',
    'aria-label': 'Pull commits (fast-forward)',
    'data-tip': 'Pull',
    on: { click: handlePull },
  }, icon('arrow-down', 'sm'));

  const head = h('div', { class: 'panel-head' },
    h('span', { class: 'panel-title branch-title' }, icon('git-branch', 'xs'), branchEl),
    syncSub,
    h('div', { class: 'panel-actions' }, pushBtn, pullBtn, refreshBtn));

  // Base selector
  const baseSelect = h('select', {
    class: 'git-base-select',
    'aria-label': 'Diff base reference',
    on: {
      change: (e) => {
        if (e.target.value === 'custom') {
          customBaseInput.hidden = false;
          customBaseInput.focus();
          currentBase = customBaseInput.value || 'HEAD';
        } else {
          customBaseInput.hidden = true;
          currentBase = e.target.value;
        }
        bus.emit('git:base', currentBase);
      },
    },
  },
  h('option', { value: 'HEAD' }, 'HEAD (Workspace)'),
  h('option', { value: 'merge-base:origin/main' }, 'merge-base:origin/main'),
  h('option', { value: 'custom' }, 'Custom ref…'));

  const customBaseInput = h('input', {
    class: 'input sm git-base-input',
    placeholder: 'e.g. main, HEAD~1',
    hidden: true,
    spellcheck: 'false',
    on: {
      change: (e) => {
        currentBase = e.target.value.trim() || 'HEAD';
        bus.emit('git:base', currentBase);
      },
      keydown: (e) => {
        if (e.key === 'Enter') {
          currentBase = e.target.value.trim() || 'HEAD';
          bus.emit('git:base', currentBase);
        }
      },
    },
  });

  const baseBar = h('div', { class: 'git-base-bar' },
    h('span', { class: 'git-base-label' }, 'Base:'),
    baseSelect,
    customBaseInput);

  // Commit box with 72-character guide
  const commitMsg = h('textarea', {
    class: 'git-commit-input',
    rows: '3',
    placeholder: `Commit message (${isMac ? '⌘' : 'Ctrl+'}Enter to commit)`,
    'aria-label': 'Commit message',
    spellcheck: 'true',
    on: {
      input: updateCharGuide,
      keydown: (e) => {
        if ((e.metaKey || e.ctrlKey) && e.key === 'Enter') {
          e.preventDefault();
          handleCommit();
        }
      },
    },
  });

  const charGuide = h('span', { class: 'git-char-count num' }, '0 / 72');
  const amendToggle = h('input', {
    type: 'checkbox',
    id: 'git-amend',
    class: 'git-amend-chk',
    'aria-label': 'Amend previous commit',
  });
  const amendLabel = h('label', { for: 'git-amend', class: 'git-amend-label' }, amendToggle, 'Amend');

  const aiMsgBtn = h('button', {
    class: 'btn sm git-ai-btn',
    type: 'button',
    'aria-label': 'Generate AI commit message',
    'data-tip': '✦ AI commit message',
    hidden: !has('ai.commit'),
    on: { click: handleAiCommitMessage },
  }, icon('sparkles', 'xs'), h('span', { class: 'git-ai-label' }, 'AI message'));

  async function handleAiCommitMessage() {
    aiMsgBtn.disabled = true;
    try {
      const { message } = await aiApi.commitMessage();
      commitMsg.value = message;
      updateCharGuide();
      commitMsg.focus();
    } catch (e) {
      if (e instanceof ApiError && e.status === 409) {
        toast({ kind: 'warn', title: 'Nothing staged', message: 'Stage changes before generating a commit message.' });
      } else if (e instanceof ApiError && e.code === 'unsupported') {
        toast({ kind: 'error', title: 'AI is not configured', message: e.message });
      } else {
        toast({ kind: 'error', title: 'Could not generate a commit message', message: e.message });
      }
    } finally {
      aiMsgBtn.disabled = false;
    }
  }

  const commitBtn = h('button', {
    class: 'btn sm primary git-commit-btn',
    type: 'button',
    'aria-label': 'Commit staged changes',
    on: { click: handleCommit },
  }, icon('git-commit', 'xs'), 'Commit');

  const commitCtrls = h('div', { class: 'git-commit-ctrls' },
    amendLabel,
    charGuide,
    h('div', { class: 'git-commit-btns' }, aiMsgBtn, commitBtn));

  const commitCard = h('div', { class: 'git-commit-card' }, commitMsg, commitCtrls);

  // Error output container
  const stderrPre = h('pre', { class: 'git-stderr-pre' }, '');
  const stderrDetails = h('details', { class: 'git-stderr', hidden: true },
    h('summary', null, 'Git output / error'),
    stderrPre);

  const listsContainer = h('div', { class: 'git-lists' });
  const recentSection = h('details', { class: 'git-recent-details' },
    h('summary', { class: 'git-recent-summary' }, 'Recent Commits'),
    h('div', { class: 'git-recent-list' }));

  const body = h('div', { class: 'panel-body git-panel-body' },
    baseBar,
    commitCard,
    stderrDetails,
    listsContainer,
    recentSection);

  mount(section, head, body);

  function updateCharGuide() {
    const firstLine = commitMsg.value.split('\n')[0] || '';
    const len = firstLine.length;
    charGuide.textContent = `${len} / 72`;
    charGuide.classList.toggle('over-budget', len > 72);
  }

  function setStderr(err) {
    if (!err) {
      stderrDetails.hidden = true;
      stderrPre.textContent = '';
      return;
    }
    const msg = err.detail?.output || err.detail?.stderr || err.message || String(err);
    stderrPre.textContent = msg;
    stderrDetails.hidden = false;
    stderrDetails.open = true;
  }

  async function handleCommit() {
    const msg = commitMsg.value.trim();
    const amend = amendToggle.checked;
    if (!msg) {
      toast({ kind: 'warn', title: 'Commit message required', message: 'Enter a commit message before committing.' });
      commitMsg.focus();
      return;
    }
    try {
      commitBtn.disabled = true;
      const res = await commitChanges(msg, amend);
      commitMsg.value = '';
      amendToggle.checked = false;
      updateCharGuide();
      setStderr(null);
      toast({ kind: 'ok', title: 'Committed successfully', message: `${res.sha.slice(0, 7)}: ${res.summary}` });
      loadRecentCommits();
    } catch (e) {
      setStderr(e);
      toast({ kind: 'error', title: 'Commit failed', message: e.message });
    } finally {
      commitBtn.disabled = false;
    }
  }

  async function handlePush() {
    try {
      pushBtn.disabled = true;
      const res = await pushRepo();
      setStderr(null);
      toast({ kind: 'ok', title: 'Pushed commits', message: res.output || 'Up to date' });
    } catch (e) {
      setStderr(e);
      toast({ kind: 'error', title: 'Push failed', message: e.message });
    } finally {
      pushBtn.disabled = false;
    }
  }

  async function handlePull() {
    try {
      pullBtn.disabled = true;
      const res = await pullRepo();
      setStderr(null);
      toast({ kind: 'ok', title: 'Pulled commits', message: res.output || 'Up to date' });
      loadRecentCommits();
    } catch (e) {
      setStderr(e);
      toast({ kind: 'error', title: 'Pull failed', message: e.message });
    } finally {
      pullBtn.disabled = false;
    }
  }

  /** Run a git action from a button; failures land in the output box and a toast, never silently. */
  async function act(title, fn) {
    try {
      await fn();
      setStderr(null);
    } catch (e) {
      setStderr(e);
      toast({ kind: 'error', title, message: e.message });
    }
  }

  function confirmDiscard(paths) {
    if (!paths.length) return;
    openDialog({
      title: `Discard changes in ${plural(paths.length, 'file')}?`,
      body: h('div', { class: 'git-discard-dialog' },
        h('p', null, 'This action is destructive and cannot be undone. Tracked changes will be restored and untracked files will be permanently deleted.'),
        h('ul', { class: 'git-discard-list' }, paths.map((p) => h('li', null, p)))),
      actions: [
        { label: 'Cancel' },
        {
          label: 'Discard changes',
          primary: true,
          run: async () => {
            try {
              await discardPaths(paths);
              toast({ kind: 'ok', title: `Discarded ${plural(paths.length, 'file')}` });
            } catch (e) {
              setStderr(e);
              toast({ kind: 'error', title: 'Discard failed', message: e.message });
            }
          },
        },
      ],
    });
  }

  async function loadRecentCommits() {
    try {
      const data = await fetchLog(10);
      const listEl = recentSection.querySelector('.git-recent-list');
      if (!listEl) return;
      if (!data?.commits?.length) {
        mount(listEl, h('p', { class: 'faint small' }, 'No recent commits'));
        return;
      }
      mount(listEl, data.commits.map((c) => h('div', { class: 'git-commit-row' },
        h('span', { class: 'git-commit-sha num' }, c.short || c.sha.slice(0, 7)),
        h('span', { class: 'git-commit-subj truncate', title: c.subject }, c.subject),
        h('span', { class: 'git-commit-author' }, c.author || ''))));
    } catch {}
  }

  function renderRow(f, groupType, activePath) {
    const isStaged = groupType === 'staged';
    const code = f.conflicted ? 'U' : (f.untracked ? '?' : (f.worktree || f.index || 'M'));
    const isCurrent = f.path === activePath;

    const row = h('div', {
      class: `list-row git-file-row${isCurrent ? ' current' : ''}`,
      title: f.path,
      on: {
        click: (e) => {
          if (e.target.closest('.lr-actions')) return;
          bus.emit('diff:open', { path: f.path, base: currentBase });
        },
      },
    },
    f.dir ? folderIcon(false) : fileIcon(f.path),
    h('span', { class: 'lr-name' }, basename(f.path)),
    h('span', { class: 'lr-dir' }, dirname(f.path)),
    h('span', { class: `gitc ${code === '?' ? 'A' : code}`, title: GIT_LABEL[code] || code }, code === '?' ? 'U' : code),
    h('div', { class: 'lr-actions' },
      isStaged
        ? h('button', {
          class: 'icon-btn xs git-unstage-btn',
          'aria-label': `Unstage ${basename(f.path)}`,
          'data-tip': 'Unstage',
          on: {
            click: (e) => {
              e.stopPropagation();
              act('Unstage failed', () => unstagePaths([f.path]));
            },
          },
        }, icon('minus', 'xs'))
        : h('button', {
          class: 'icon-btn xs git-stage-btn',
          'aria-label': `Stage ${basename(f.path)}`,
          'data-tip': 'Stage',
          on: {
            click: (e) => {
              e.stopPropagation();
              act('Stage failed', () => stagePaths([f.path]));
            },
          },
        }, icon('plus', 'xs')),
      h('button', {
        class: 'icon-btn xs git-discard-btn',
        'aria-label': `Discard changes in ${basename(f.path)}`,
        'data-tip': 'Discard',
        on: {
          click: (e) => {
            e.stopPropagation();
            confirmDiscard([f.path]);
          },
        },
      }, icon('x', 'xs'))));

    return row;
  }

  function renderGroup(title, groupType, files, activePath) {
    if (!files.length) return null;
    const paths = files.map((f) => f.path);
    const isStaged = groupType === 'staged';

    const header = h('div', { class: 'list-group-head git-group-head' },
      h('span', { class: 'git-group-title' }, title),
      h('span', { class: 'count num' }, String(files.length)),
      h('div', { class: 'git-group-actions' },
        isStaged
          ? h('button', {
            class: 'icon-btn xs',
            'aria-label': 'Unstage all',
            'data-tip': 'Unstage all',
            on: { click: () => act('Unstage failed', () => unstagePaths(paths)) },
          }, icon('minus', 'xs'))
          : h('button', {
            class: 'icon-btn xs',
            'aria-label': 'Stage all',
            'data-tip': 'Stage all',
            on: { click: () => act('Stage failed', () => stagePaths(paths)) },
          }, icon('plus', 'xs')),
        h('button', {
          class: 'icon-btn xs',
          'aria-label': 'Discard all',
          'data-tip': 'Discard all',
          on: { click: () => confirmDiscard(paths) },
        }, icon('x', 'xs'))));

    return h('div', { class: 'list-group' },
      header,
      files.map((f) => renderRow(f, groupType, activePath)));
  }

  function render(g) {
    if (!g) {
      branchEl.textContent = 'No repository';
      syncSub.textContent = '';
      mount(listsContainer, h('div', { class: 'empty' },
        icon('git-branch', 'xl'),
        h('h3', null, 'No git repository'),
        h('p', null, 'Open a folder inside a git repository to view and manage changes.')));
      return;
    }

    branchEl.textContent = g.branch || (g.detached ? 'detached HEAD' : 'Changes');
    syncSub.textContent = (g.ahead || g.behind) ? `↑${g.ahead || 0} ↓${g.behind || 0}` : '';

    const files = g.files || [];
    const activePath = store.get('active');

    const conflicts = files.filter((f) => f.conflicted);
    const staged = files.filter((f) => f.index && !f.untracked && !f.conflicted);
    const unstaged = files.filter((f) => f.worktree && !f.untracked && !f.conflicted);
    const untracked = files.filter((f) => f.untracked);

    const totalChanges = staged.length + unstaged.length + untracked.length + conflicts.length;

    if (!totalChanges) {
      mount(listsContainer, h('div', { class: 'empty' },
        icon('check-circle', 'xl'),
        h('h3', null, 'Working tree clean'),
        h('p', null, `No changes on ${g.branch || 'this branch'}.`)));
      return;
    }

    mount(listsContainer, [
      renderGroup('Conflicts', 'conflicts', conflicts, activePath),
      renderGroup('Staged Changes', 'staged', staged, activePath),
      renderGroup('Changes', 'unstaged', unstaged, activePath),
      renderGroup('Untracked', 'untracked', untracked, activePath),
    ].filter(Boolean));
  }

  store.subscribe('git', render, { now: true });
  store.subscribe('active', () => render(store.get('git')));
  loadRecentCommits();
  } // end buildGitPanel
}
