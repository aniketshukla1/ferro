// Settings dialog: section nav + a form generated from GET /settings/schema plus the
// frontend's own ui.* keys. Edits save immediately to the chosen scope (user | workspace);
// "Reset" sends null for that key. Live effects (theme, font size, icon tint) apply at once.
import { h, mount } from '../core/dom.js';
import { api, has } from '../core/api.js';
import { store } from '../core/store.js';
import { debounce } from '../core/util.js';
import { icon } from '../ui/icons.js';
import { openDialog, toast } from '../ui/overlay.js';
import { THEMES, currentTheme, resolveTheme, setTheme } from './themes.js';

/** Frontend-owned settings (API.md § 3.2: the backend stores ui.* without reading it). */
export const UI_KEYS = [
  { key: 'ui.theme', section: 'appearance', title: 'Color theme', description: 'Auto follows your system light or dark setting.', type: 'theme', default: 'auto', scopes: ['user'] },
  { key: 'ui.codeFontSize', section: 'appearance', title: 'Code font size', description: 'Pixel size of code in the viewer and diffs.', type: 'int', min: 10, max: 20, default: 13, scopes: ['user', 'workspace'] },
  { key: 'ui.fileIconColors', section: 'appearance', title: 'Tinted file icons', description: 'Color file icons by language instead of keeping them neutral.', type: 'bool', default: false, scopes: ['user', 'workspace'] },
  { key: 'ui.markdownPreview', section: 'editor', title: 'Open Markdown as preview', description: 'Show rendered Markdown first. Alt+M switches to the source.', type: 'bool', default: true, scopes: ['user', 'workspace'] },
  { key: 'ui.autoReveal', section: 'editor', title: 'Reveal the active file', description: 'Keep the file tree scrolled to the file you are viewing.', type: 'bool', default: true, scopes: ['user', 'workspace'] },
];

const SECTIONS = [
  ['appearance', 'Appearance', 'contrast'],
  ['editor', 'Editor', 'file-code'],
  ['files', 'Files', 'files'],
  ['search', 'Search', 'search'],
  ['review', 'Review', 'git-pull-request'],
  ['ai', 'AI', 'sparkles'],
  ['harness', 'Agents', 'terminal'],
  ['security', 'Security', 'lock'],
  ['updates', 'Updates', 'refresh'],
];

/** Settings → Security: remembered-browser status and sign-out (not a stored setting). */
const SECURITY_DEF = {
  key: 'auth.browsers',
  section: 'security',
  title: 'Signed-in browsers',
  description: 'Remember this browser, sign out of this browser, sign out of all browsers.',
  type: 'security',
};

async function signOut(everywhere) {
  try {
    await api.logout(everywhere);
    location.reload();
  } catch (e) {
    toast({ kind: 'error', title: 'Could not sign out', message: e.message });
  }
}

function securityField(d) {
  const auth = store.get('meta')?.auth;
  const days = auth?.remember ? auth.rememberDays : 0;
  const status = days
    ? `This browser stays signed in for ${days} days after each visit, even when ferro restarts. It applies to 127.0.0.1 and localhost on this computer.`
    : 'This browser stays signed in until you close it. Remembering browsers is off because this ferro can be reached from other computers.';
  const one = h('button', { class: 'btn', on: { click: () => signOut(false) } }, icon('lock', 'sm'), 'Sign out of this browser');
  const allLabel = h('span', null, 'Sign out of all browsers');
  let armedAt = 0;
  const all = h('button', {
    class: 'btn',
    on: {
      click: () => {
        if (Date.now() - armedAt < 5000) { signOut(true); return; }
        // Two clicks: this also ends every other open session.
        armedAt = Date.now();
        all.classList.add('danger');
        allLabel.textContent = 'Click again to sign out everywhere';
        setTimeout(() => {
          if (Date.now() - armedAt >= 5000) { all.classList.remove('danger'); allLabel.textContent = 'Sign out of all browsers'; }
        }, 5100);
      },
    },
  }, icon('lock', 'sm'), allLabel);
  return h('div', { class: 'field security-field' },
    h('div', { class: 'f-title' }, d.title),
    h('p', { class: 'f-desc' }, status),
    h('p', { class: 'f-desc' }, 'Signing out of all browsers ends every other open session too. The link ferro printed when it started signs you back in.'),
    h('div', { class: 'row sec-actions' }, one, all));
}

// Backend keys for the legacy UI (un-namespaced) are not shown in the new UI.
const hiddenKey = (k) => !k.key.includes('.') || k.section === 'ui';

const tok = (cls, text) => h('span', { class: cls }, text);
function preview() {
  return h('div', { class: 'tc-code', 'aria-hidden': 'true' },
    h('div', null, tok('t-k', 'fn'), ' ', tok('t-fd', 'review'), tok('t-p', '('), tok('t-vp', 'pr'), tok('t-p', ':'), ' ', tok('t-t', 'Diff'), tok('t-p', ') {')),
    h('div', null, '  ', tok('t-c', '// fast by default')),
    h('div', null, '  ', tok('t-k', 'let'), ' ms ', tok('t-o', '='), ' ', tok('t-n', '3.1'), tok('t-p', ';')),
    h('div', null, '  ', tok('t-m', 'println!'), tok('t-p', '('), tok('t-s', '"ok"'), tok('t-p', ')')),
    h('div', null, tok('t-p', '}')));
}

function themeGallery() {
  const grid = h('div', { class: 'theme-grid', role: 'radiogroup', 'aria-label': 'Color theme' });
  const all = [{ id: 'auto', name: 'Auto', type: 'system' }, ...THEMES];
  // Cards are built once and updated in place (re-rendering would drop focus and clicks).
  const cards = all.map((t) => {
    const check = h('span', { class: 'tc-check' }, icon('check', 'sm'));
    const card = h('button', { class: 'theme-card', role: 'radio', on: { click: () => { setTheme(t.id); sync(); } } },
      h('div', { class: 'tc-preview' }, preview()),
      h('div', { class: 'tc-foot' }, h('span', { class: 'tc-name' }, t.name), h('span', { class: 'tc-type' }, t.note || t.type), check));
    card.querySelector('.tc-preview').dataset.theme = resolveTheme(t.id);
    card.__id = t.id;
    card.__check = check;
    return card;
  });
  function sync() {
    const cur = currentTheme();
    for (const c of cards) {
      c.setAttribute('aria-checked', String(c.__id === cur));
      c.__check.hidden = c.__id !== cur;
    }
  }
  mount(grid, cards);
  sync();
  return { el: grid, focus: () => cards.find((c) => c.getAttribute('aria-checked') === 'true')?.focus() };
}

export async function openSettings({ section = 'appearance' } = {}) {
  let schema = [];
  let scope = 'user';
  let scoped = {}; // explicit values in the current scope
  let effective = store.get('settings') || {};
  try {
    const [s, sc] = await Promise.all([api.settingsSchema(), api.settings(scope)]);
    schema = s.keys.filter((k) => !hiddenKey(k));
    scoped = sc.values || {};
  } catch (e) {
    toast({ kind: 'error', title: 'Settings are unavailable', message: e.message });
  }
  const defs = [...UI_KEYS, ...schema, ...(has('auth.logout') ? [SECURITY_DEF] : [])];
  const present = new Set(defs.map((d) => d.section));
  const sections = SECTIONS.filter(([id]) => present.has(id));
  for (const d of defs) if (!SECTIONS.some(([id]) => id === d.section) && !sections.some(([id]) => id === d.section)) sections.push([d.section, d.section[0].toUpperCase() + d.section.slice(1), 'sliders']);

  const filter = h('input', { class: 'input', type: 'search', placeholder: 'Search settings', 'aria-label': 'Search settings' });
  const navList = h('div', { class: 'col' });
  const nav = h('nav', { class: 'set-nav', 'aria-label': 'Settings sections' }, filter, navList);
  const main = h('div', { class: 'set-main' });
  let current = sections.some(([id]) => id === section) ? section : sections[0]?.[0];

  const scopeSeg = h('div', { class: 'seg', role: 'group', 'aria-label': 'Scope' },
    h('button', { 'aria-pressed': 'true', 'data-scope': 'user', on: { click: () => setScope('user') } }, 'User'),
    h('button', { 'aria-pressed': 'false', 'data-scope': 'workspace', on: { click: () => setScope('workspace') } }, 'Workspace'));

  async function setScope(next) {
    scope = next;
    for (const b of scopeSeg.children) b.setAttribute('aria-pressed', String(b.dataset.scope === scope));
    try { scoped = (await api.settings(scope)).values || {}; } catch { scoped = {}; }
    renderMain();
  }

  async function save(key, value) {
    try {
      const res = await api.putSettings({ [key]: value }, scope);
      effective = res.values || effective;
      store.set('settings', effective);
      if (value === null) delete scoped[key];
      else scoped[key] = value;
      renderNav();
      return true;
    } catch (e) {
      toast({ kind: 'error', title: `Could not save ${key}`, message: e.detail?.reason || e.message });
      return false;
    }
  }

  function valueOf(d) {
    if (d.key in scoped) return scoped[d.key];
    if (d.key in effective) return effective[d.key];
    return d.default;
  }

  function control(d) {
    const v = valueOf(d);
    if (d.type === 'bool') {
      const sw = h('button', { class: 'switch', role: 'switch', 'aria-checked': String(!!v), 'aria-label': d.title });
      sw.addEventListener('click', async () => {
        const next = sw.getAttribute('aria-checked') !== 'true';
        sw.setAttribute('aria-checked', String(next));
        if (!(await save(d.key, next))) sw.setAttribute('aria-checked', String(!next));
        else renderMain();
      });
      return sw;
    }
    if (d.type === 'enum') {
      const sel = h('select', { class: 'input', 'aria-label': d.title }, d.enum.map((o) => h('option', { value: o }, o)));
      sel.value = String(v);
      sel.addEventListener('change', async () => { if (await save(d.key, sel.value)) renderMain(); });
      return sel;
    }
    if (d.type === 'int') {
      const inp = h('input', { class: 'input num', type: 'number', 'aria-label': d.title, min: d.min ?? '', max: d.max ?? '', step: '1' });
      inp.value = String(v ?? '');
      inp.addEventListener('change', async () => {
        const n = Math.round(Number(inp.value));
        if (!Number.isFinite(n) || (d.min != null && n < d.min) || (d.max != null && n > d.max)) {
          toast({ kind: 'warn', title: `${d.title}: enter ${d.min ?? '…'}–${d.max ?? '…'}` });
          inp.value = String(valueOf(d));
          return;
        }
        if (await save(d.key, n)) renderMain();
      });
      return inp;
    }
    if (d.type === 'string[]') {
      const ta = h('textarea', { class: 'input mono', rows: '3', spellcheck: 'false', 'aria-label': `${d.title}, one per line` });
      ta.value = (Array.isArray(v) ? v : []).join('\n');
      const commit = debounce(async () => {
        const list = ta.value.split('\n').map((s) => s.trim()).filter(Boolean);
        await save(d.key, list);
      }, 600);
      ta.addEventListener('input', commit);
      ta.addEventListener('blur', () => { commit.cancel?.(); commit(); });
      return ta;
    }
    const inp = h('input', { class: 'input', type: 'text', spellcheck: 'false', 'aria-label': d.title, placeholder: d.default ? String(d.default) : 'default' });
    inp.value = String(v ?? '');
    inp.addEventListener('change', () => save(d.key, inp.value));
    return inp;
  }

  function field(d) {
    const explicit = d.key in scoped;
    const scopeOk = !d.scopes || d.scopes.includes(scope);
    const reset = explicit ? h('button', { class: 'btn ghost sm', 'data-tip': `Reset to ${JSON.stringify(d.default)}`, on: { click: async () => { if (await save(d.key, null)) { if (d.key === 'ui.theme') setTheme('auto', { persist: false }); renderMain(); } } } }, 'Reset') : null;
    return h('div', { class: `field${explicit ? ' modified' : ''}`, 'data-key': d.key },
      h('div', { class: 'f-title' }, d.title),
      h('div', { class: 'f-control' }, reset, scopeOk ? control(d) : h('span', { class: 'faint small' }, 'User scope only')),
      d.description ? h('div', { class: 'f-desc' }, d.description) : null,
      h('div', { class: 'f-key' }, d.key));
  }

  function matchesFilter(d) {
    const q = filter.value.trim().toLowerCase();
    return !q || `${d.key} ${d.title} ${d.description || ''}`.toLowerCase().includes(q);
  }

  let gallery = null;
  function renderMain() {
    const q = filter.value.trim();
    const list = defs.filter((d) => (q ? matchesFilter(d) : d.section === current));
    // Security holds actions, not stored settings: no scope switch there.
    const out = list.some((d) => d.type !== 'security')
      ? [h('div', { class: 'set-scope' }, h('span', null, 'Saving to'), scopeSeg,
        h('span', null, scope === 'user' ? 'for every workspace' : 'for this workspace only'))]
      : [];
    if (!list.length) out.push(h('div', { class: 'empty' }, h('p', null, 'No settings match.')));
    let lastSection = null;
    for (const d of list) {
      if (q && d.section !== lastSection) {
        lastSection = d.section;
        out.push(h('h3', { class: 'label' }, sections.find(([id]) => id === d.section)?.[1] || d.section));
      }
      if (d.type === 'theme') {
        gallery = themeGallery();
        out.push(h('div', { class: 'field theme-field' }, h('div', { class: 'f-title' }, d.title), h('div', { class: 'f-desc' }, d.description), gallery.el));
      } else if (d.type === 'security') {
        out.push(securityField(d));
      } else out.push(field(d));
    }
    mount(main, out);
  }

  function renderNav() {
    const q = filter.value.trim();
    mount(navList, sections.map(([id, title, ic]) => {
      const n = defs.filter((d) => d.section === id && (!q || matchesFilter(d))).length;
      const b = h('button', { 'aria-current': String(!q && id === current), on: { click: () => { current = id; filter.value = ''; renderNav(); renderMain(); } } }, icon(ic, 'sm'), h('span', { class: 'grow' }, title));
      if (q) b.appendChild(h('span', { class: 'count' }, String(n)));
      b.hidden = !!q && !n;
      return b;
    }));
  }

  filter.addEventListener('input', () => { renderNav(); renderMain(); });
  renderNav();
  renderMain();
  const dlg = openDialog({ title: 'Settings', className: 'settings-dialog', body: h('div', { class: 'settings-body' }, nav, main) });
  if (current === 'appearance') gallery?.focus();
  return dlg;
}
