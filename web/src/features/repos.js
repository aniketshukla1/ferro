// Open a repository (API.md § 7.2): a folder opened before, any folder by its path (the desktop
// app asks with the system picker), or a pull request by its link. The server switches roots in
// place, and its `workspace` event moves the whole UI over, as when a pull request opens.
import { h } from '../core/dom.js';
import { request, has } from '../core/api.js';
import { store } from '../core/store.js';
import { execute } from '../core/commands.js';
import { icon } from '../ui/icons.js';
import { toast } from '../ui/overlay.js';
import { openDialog } from '../ui/dialog.js';

const PR_LINK = /^https?:\/\/\S+\/(pull\/\d+|-\/merge_requests\/\d+)/;

/** The picker, in the palette: folders opened before, then a folder by path, then a pull request. */
export async function open(palette) {
  if (store.get('meta')?.readOnly) {
    toast({ title: 'This ferro is read-only', message: 'Opening another repository is turned off.' });
    return;
  }
  const recent = has('workspace.recent') ? (await request('workspace/recent').catch(() => null))?.items || [] : [];
  const items = recent.filter((r) => !r.current).map((r) => ({
    key: `repo:${r.root}`,
    icon: icon(r.git ? 'git-branch' : 'folder', 'sm'),
    label: r.name,
    desc: r.root,
    text: `${r.name} ${r.root}`,
    run: () => switchTo(r.root).catch((e) => toast({ kind: 'error', title: `Could not open ${r.name}`, message: e.message })),
  }));
  items.push(
    { key: 'repo:folder', icon: icon('folder-open', 'sm'), label: 'Open Folder…', desc: store.get('meta')?.host === 'desktop' ? 'choose a folder' : 'type its path', text: 'open folder path', run: openFolder },
    { key: 'repo:pr', icon: icon('git-pull-request', 'sm'), label: 'Open Pull Request…', desc: 'paste a GitHub or GitLab link', text: 'open pull request merge request link', run: openPr },
  );
  palette.open('', { special: 'pick', title: 'Open repository', items });
}

async function switchTo(path) {
  await request('workspace/open', { method: 'POST', body: { path } });
  toast({ kind: 'ok', title: `Opened ${path.split(/[\\/]/).filter(Boolean).pop() || path}`, message: path, timeout: 2500 });
}

function openFolder() {
  if (store.get('meta')?.host === 'desktop') {
    request('desktop/pick-folder', { method: 'POST' })
      .then(({ path }) => path && switchTo(path))
      .catch((e) => toast({ kind: 'error', title: 'Could not open the folder', message: e.message }));
    return;
  }
  ask({
    title: 'Open folder',
    label: 'Folder path',
    placeholder: '~/code/project, or a full path',
    submit: (path) => switchTo(path).then(() => null, (e) => e.message),
  });
}

function openPr() {
  ask({
    title: 'Open pull request',
    label: 'Pull request link',
    placeholder: 'https://github.com/owner/repo/pull/123',
    submit: (url) => {
      if (!PR_LINK.test(url)) return 'Paste a GitHub pull request or GitLab merge request link.';
      execute('pr.open', url); // its toast follows the checkout
      return null;
    },
  });
}

/** A one-field dialog. `submit` resolves to an error to show, or null when done. */
function ask({ title, label, placeholder, submit }) {
  const input = h('input', { class: 'input mono', type: 'text', spellcheck: 'false', autocomplete: 'off', placeholder });
  const err = h('p', { class: 'agent-error', role: 'alert', hidden: true });
  const go = async () => {
    const value = input.value.trim();
    if (!value) return false;
    const msg = await submit(value);
    err.textContent = msg || '';
    err.hidden = !msg;
    return !msg;
  };
  const d = openDialog({
    title,
    className: 'repo-dialog',
    width: 'min(560px, calc(100vw - 32px))',
    body: h('div', { class: 'col' }, h('label', { class: 'col' }, h('span', { class: 'f-title' }, label), input), err),
    actions: [{ label: 'Cancel' }, { label: 'Open', primary: true, run: go }],
  });
  input.addEventListener('keydown', (e) => {
    if (e.key === 'Enter') { e.preventDefault(); go().then((ok) => ok && d.close()); }
  });
}
