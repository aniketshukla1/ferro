// Team review memory (API.md § 17, FRONTEND.md § 6.24): what reviewers dismiss and accept
// becomes rules. "Don't report this again" rules hide a kind of finding (in some paths or
// everywhere); conventions tell the AI reviewer what this team cares about. Team rules live in
// .ferro-rules.json (shared through git); personal ones stay on this machine. Loads on first use.
import { h, mount } from '../core/dom.js';
import { request } from '../core/api.js';
import { bus } from '../core/bus.js';
import { plural, relTime } from '../core/util.js';
import { icon } from '../ui/icons.js';
import { toast } from '../ui/overlay.js';
import { openDialog } from '../ui/dialog.js';

const dirOf = (p) => (p && p.includes('/') ? p.slice(0, p.lastIndexOf('/')) : '');

/** Plain words for what a rule matches. */
export function describe(r) {
  if (r.kind === 'convention') return r.text;
  const what = r.rule
    ? (r.rule.endsWith('*') ? `every ${r.rule.slice(0, -1).replace(/\.$/, '')} finding` : r.rule)
    : r.title
      ? `“${r.title}”${r.category ? ` (${r.category})` : ''}`
      : `every ${r.category} finding`;
  const where = r.paths?.length ? ` in ${r.paths.join(', ')}` : ' anywhere';
  const src = r.appliesTo === 'security' ? 'Security: ' : r.appliesTo === 'ai' ? 'AI review: ' : '';
  return `${src}don’t report ${what}${where}`;
}

/**
 * Create a rule. `prefill`: { kind, appliesTo, rule?, category?, title?, path?, paths?, text? }.
 * Resolves with the saved rule, or null when cancelled.
 */
export function openRuleDialog(prefill = {}) {
  return new Promise((resolve) => {
    let done = false;
    const finish = (v) => { if (!done) { done = true; resolve(v); } };
    const convention = prefill.kind === 'convention';
    const radio = (name, value, label, checked) => h('label', { class: 'mem-opt' }, h('input', { type: 'radio', name, value, checked: checked || undefined }), h('span', null, label));
    const pick = (name) => body.querySelector(`input[name="${name}"]:checked`)?.value;
    const path = prefill.path || '';
    const dir = dirOf(path);
    const family = prefill.rule && prefill.rule.includes('.') ? `${prefill.rule.split('.')[0]}.*` : null;
    const text = h('textarea', { class: 'input', rows: '2', placeholder: 'e.g. Every public function has a doc comment', 'aria-label': 'Convention' }, prefill.text || '');
    const reason = h('input', { class: 'input', placeholder: convention ? 'Optional: why it matters' : 'Why? e.g. test fixtures, not real keys', 'aria-label': 'Reason' });
    const custom = h('input', { class: 'input sm', placeholder: 'glob, e.g. tests/**', 'aria-label': 'Custom paths' });
    const err = h('p', { class: 'agent-error', role: 'alert', hidden: true });
    const body = h('div', { class: 'mem-form' },
      convention
        ? h('label', { class: 'hc-field' }, h('span', { class: 'faint small' }, 'Convention'), text)
        : h('div', { class: 'hc-field' }, h('span', { class: 'faint small' }, 'What'),
          prefill.rule
            ? h('div', { class: 'mem-opts' }, radio('what', prefill.rule, `This rule (${prefill.rule})`, true), family ? radio('what', family, `Every ${family.slice(0, -2)} rule`) : null)
            : h('p', { class: 'mem-what' }, prefill.title ? `“${prefill.title}”` : '', prefill.category ? h('span', { class: 'faint' }, ` · ${prefill.category}`) : '')),
      convention ? null : h('div', { class: 'hc-field' }, h('span', { class: 'faint small' }, 'Where'),
        h('div', { class: 'mem-opts' },
          path ? radio('where', 'file', `This file (${path})`, !prefill.paths) : null,
          dir ? radio('where', 'dir', `This folder (${dir}/**)`) : null,
          prefill.paths?.length ? radio('where', 'suggested', prefill.paths.join(', '), true) : null,
          radio('where', 'all', 'Everywhere', !path && !prefill.paths?.length),
          h('label', { class: 'mem-opt' }, h('input', { type: 'radio', name: 'where', value: 'custom' }), custom))),
      h('label', { class: 'hc-field' }, h('span', { class: 'faint small' }, convention ? 'Why (optional)' : 'Reason'), reason),
      h('div', { class: 'hc-field' }, h('span', { class: 'faint small' }, 'Who'),
        h('div', { class: 'mem-opts' },
          radio('scope', 'personal', 'Just me', true),
          radio('scope', 'team', 'My team: saved in .ferro-rules.json; commit it to share'))),
      err);
    custom.addEventListener('focus', () => { body.querySelector('input[name="where"][value="custom"]').checked = true; });
    openDialog({
      title: convention ? 'Add a team convention' : 'Don’t report this again',
      className: 'mem-dialog',
      width: 'min(540px, calc(100vw - 32px))',
      body,
      onClose: () => finish(null),
      actions: [{ label: 'Cancel' }, {
        label: 'Save rule',
        primary: true,
        run: async () => {
          const where = pick('where');
          const paths = convention ? [] : where === 'file' ? [path] : where === 'dir' ? [`${dir}/**`] : where === 'suggested' ? prefill.paths : where === 'custom' ? custom.value.split(',').map((s) => s.trim()).filter(Boolean) : [];
          const what = pick('what');
          const payload = {
            kind: convention ? 'convention' : 'ignore',
            appliesTo: prefill.appliesTo || 'ai',
            rule: convention ? undefined : what || prefill.rule,
            category: convention ? prefill.category : prefill.rule ? undefined : prefill.category,
            title: convention || prefill.rule ? undefined : prefill.title,
            text: convention ? text.value.trim() : undefined,
            paths,
            reason: reason.value.trim(),
            scope: pick('scope'),
          };
          try {
            const rule = await request('memory/rules', { method: 'POST', body: payload });
            toast({ kind: 'ok', title: payload.scope === 'team' ? 'Team rule saved' : 'Rule saved', message: payload.scope === 'team' ? 'Commit .ferro-rules.json to share it.' : describe(rule), timeout: 3000 });
            bus.emit('memory:changed');
            finish(rule);
          } catch (e) {
            err.textContent = e.message;
            err.hidden = false;
            return false;
          }
          return true;
        },
      }],
    });
    (convention ? text : reason).focus();
  });
}

export function renderMemoryTab(el) {
  const addBtn = h('button', { class: 'btn sm', on: { click: () => openRuleDialog({ kind: 'convention', appliesTo: 'ai' }) } }, icon('plus', 'sm'), 'Convention');
  const info = h('p', { class: 'faint small mem-info' });
  const sugEl = h('div', { class: 'mem-sugs' });
  const teamEl = h('div', { class: 'mem-group' });
  const meEl = h('div', { class: 'mem-group' });
  mount(el, h('div', { class: 'mem' },
    h('div', { class: 'ck-top' }, icon('layers', 'sm'), h('span', { class: 'ck-title' }, 'Team review memory'), h('span', { class: 'ck-sp' }), addBtn),
    info, sugEl, teamEl, meEl));

  async function load() {
    let m;
    try {
      m = await request('memory');
    } catch (e) {
      mount(teamEl, h('p', { class: 'faint small' }, e.message));
      return;
    }
    info.textContent = m.team.source === 'base'
      ? 'Pull request: team rules come from the base branch, so a change cannot hide its own findings.'
      : m.team.gitIgnored
        ? `Warning: git ignores ${m.team.path}, so your team will never get these rules. Remove the matching line from .gitignore (or add !${m.team.path}).`
        : `Team rules live in ${m.team.path}${m.team.exists ? '' : ' (not created yet)'}: commit it so everyone gets them. Personal rules stay on this machine.`;
    info.classList.toggle('agent-error', !!m.team.gitIgnored);
    mount(sugEl, m.suggestions.length ? [h('div', { class: 'mem-head' }, 'Suggested'), m.suggestions.map((s) => h('div', { class: 'mem-sug' },
      h('div', { class: 'mem-line' }, h('span', { class: `mem-kind ${s.rule.kind}` }, s.rule.kind === 'ignore' ? 'Ignore' : 'Convention'), h('span', null, describe(s.rule))),
      h('div', { class: 'faint small' }, `${s.why}: ${s.examples.join(', ')}`),
      h('div', { class: 'row mem-actions' },
        h('button', { class: 'btn sm', on: { click: async () => { if (await openRuleDialog({ ...s.rule, path: s.examples[0], paths: s.rule.paths?.length ? s.rule.paths : undefined })) load(); } } }, 'Create rule…'),
        h('button', { class: 'btn ghost sm', on: { click: async () => { await request('memory/suggestions/dismiss', { method: 'POST', body: { key: s.key } }).catch(() => {}); load(); } } }, 'Not now'))))] : null);
    const rows = (list) => list.map((r) => h('div', { class: 'mem-rule' },
      h('div', { class: 'mem-line' }, h('span', { class: `mem-kind ${r.kind}` }, r.kind === 'ignore' ? 'Ignore' : 'Convention'), h('span', { class: 'mem-desc' }, describe(r))),
      r.reason ? h('div', { class: 'mem-reason small' }, r.reason) : null,
      h('div', { class: 'faint small mem-meta' },
        [r.author, r.createdAt ? relTime(r.createdAt) : '', r.kind === 'ignore' ? (r.hits ? `hid ${plural(r.hits, 'finding')}` : 'nothing hidden yet') : ''].filter(Boolean).join(' · ')),
      h('div', { class: 'row mem-actions' },
        h('button', { class: 'btn ghost sm', on: { click: () => move(r, r.scope === 'team' ? 'personal' : 'team') } }, r.scope === 'team' ? 'Make personal' : 'Share with team'),
        h('button', { class: 'btn ghost sm', on: { click: () => remove(r) } }, 'Delete'))));
    const team = m.rules.filter((r) => r.scope === 'team');
    const mine = m.rules.filter((r) => r.scope === 'personal');
    mount(teamEl, h('div', { class: 'mem-head' }, `Team · ${team.length}`), team.length ? rows(team) : h('p', { class: 'faint small' }, 'No team rules yet. Share a rule, or add a convention.'));
    mount(meEl, h('div', { class: 'mem-head' }, `Just me · ${mine.length}`), mine.length ? rows(mine) : h('p', { class: 'faint small' }, 'Dismiss a finding and choose “Don’t report again” to add one.'));
  }

  async function move(r, scope) {
    try {
      await request(`memory/rules/${encodeURIComponent(r.id)}`, { method: 'PATCH', body: { scope } });
      toast({ kind: 'ok', title: scope === 'team' ? 'Shared with the team' : 'Now personal', message: scope === 'team' ? 'Commit .ferro-rules.json to share it.' : undefined, timeout: 2500 });
      bus.emit('memory:changed');
    } catch (e) {
      toast({ kind: 'error', title: 'Could not move the rule', message: e.message });
    }
  }

  function remove(r) {
    openDialog({
      title: 'Delete this rule?',
      body: h('p', null, describe(r)),
      actions: [{ label: 'Cancel' }, {
        label: 'Delete',
        primary: true,
        run: async () => {
          try {
            await request(`memory/rules/${encodeURIComponent(r.id)}`, { method: 'DELETE' });
            bus.emit('memory:changed');
          } catch (e) {
            toast({ kind: 'error', title: 'Could not delete the rule', message: e.message });
            return false;
          }
          return true;
        },
      }],
    });
  }

  const off = bus.on('memory:changed', load);
  load();
  return { destroy: off };
}
