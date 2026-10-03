// First-run tips on the home screen (FRONTEND.md § 6.17): what ferro does beyond browsing,
// each with the action that starts it. Hidden for good once dismissed.
import { h, mount } from '../core/dom.js';
import { has } from '../core/api.js';
import { keysEl } from '../core/keys.js';
import { execute } from '../core/commands.js';
import { storage } from '../core/util.js';
import { icon } from '../ui/icons.js';

export const TIPS_KEY = 'ferro.tips.dismissed';

export function renderTips(slot) {
  const tips = [
    { ic: 'search', title: 'Find anything', body: 'Files by name, > commands, # symbols, % text.', keys: 'Mod+K', cmd: 'palette.files', label: 'Open the palette' },
    has('workspace.open') && { ic: 'folder-open', title: 'Open a project', body: 'A recent project, any folder, or a pull request. The project name at the top left opens it too.', keys: 'Mod+Alt+O', cmd: 'workspace.switch', label: 'Open a project' },
    has('pr.open') && { ic: 'git-pull-request', title: 'Review a pull request', body: 'Paste a GitHub or GitLab link: ferro checks it out beside your work.', cmd: 'pr.open', label: 'Open a pull request' },
    has('ai') && { ic: 'sparkles', title: 'Ask and review with AI', body: 'Questions about the code, review findings in the diff, commit messages.', keys: 'Mod+I', cmd: 'ai.ask', label: 'Ask AI' },
    has('nav') && { ic: 'enter', title: 'Jump to definitions', body: 'F12 or Mod+click a name; Shift+F12 lists its references.', keys: 'F12' },
    has('harness') && { ic: 'terminal', title: 'Edit with an agent', body: 'Select lines, press Alt+E, review the change hunk by hunk.', keys: 'Alt+E' },
  ].filter(Boolean).slice(0, 4);
  const dismiss = h('button', { class: 'icon-btn sm home-tips-x', 'aria-label': 'Dismiss tips', 'data-tip': 'Dismiss tips', on: { click: () => { storage.set(TIPS_KEY, true); slot.hidden = true; mount(slot); } } }, icon('x', 'sm'));
  mount(slot, h('section', { class: 'home-tips', 'aria-label': 'Getting started' },
    h('div', { class: 'home-col-head' }, h('h2', { class: 'label' }, 'Getting started'), dismiss),
    h('ul', { class: 'home-tips-list' }, tips.map((t) => h('li', { class: 'home-tip' },
      h('span', { class: 'ht-icon' }, icon(t.ic, 'sm')),
      h('div', { class: 'ht-body' },
        h('div', { class: 'ht-title' }, t.title, t.keys ? keysEl(t.keys) : null),
        h('p', { class: 'ht-text' }, t.body),
        t.cmd ? h('button', { class: 'btn ghost sm ht-action', on: { click: () => execute(t.cmd) } }, t.label, icon('chevron-right', 'xs')) : null))))));
  slot.hidden = false;
}
