// On-demand chrome: keyboard help sheet, auth screen, inspector tabs (the connection banner is in connection.js).
import { h, mount } from '../core/dom.js';
import { keysEl } from '../core/keys.js';
import { listCommands, execute } from '../core/commands.js';
import { store } from '../core/store.js';
import { formatCount, formatMs, formatBytes, isMac } from '../core/util.js';
import { icon, brandMark } from '../ui/icons.js';
import { tokenFromInput } from '../core/text.js';
import { openDialog } from '../ui/overlay.js';

// ---------- keyboard help ----------
// [category, title, key spec or null, literal text shown instead of key caps]
const EXTRA = [
  ['Palette', 'Commands mode', null, '>'],
  ['Palette', 'Symbols in file', null, '@'],
  ['Palette', 'Go to line', null, ':'],
  ['Palette', 'Search text', null, '%'],
  ['Palette', 'Open a file at a line', null, 'name:42'],
  ['Viewer', 'Extend line selection', 'Shift+ArrowDown'],
  ['Viewer', 'Top / bottom of file', 'Mod+ArrowUp'],
  ['Viewer', 'Select lines', null, 'drag the line numbers'],
  ['Explorer', 'Expand / collapse folder', 'ArrowRight'],
  ['Explorer', 'Jump by typing', null, 'type a name'],
];

export function showKeyboardHelp() {
  const filter = h('input', { class: 'input', type: 'search', placeholder: 'Filter shortcuts', 'aria-label': 'Filter shortcuts' });
  const list = h('div', { class: 'keys-sheet' });
  function render() {
    const q = filter.value.trim().toLowerCase();
    const rows = [];
    for (const c of listCommands()) if (c.keys?.length) rows.push([c.category || 'General', c.title, c.keys[0], null]);
    rows.push(...EXTRA);
    const groups = new Map();
    for (const [cat, title, keys, text] of rows) {
      if (q && !`${cat} ${title}`.toLowerCase().includes(q)) continue;
      if (!groups.has(cat)) groups.set(cat, []);
      groups.get(cat).push([title, keys, text]);
    }
    mount(list, [...groups].map(([cat, items]) => h('section', { class: 'ks-group' },
      h('h3', { class: 'label' }, cat),
      items.map(([title, keys, text]) => h('div', { class: 'ks-row' }, h('span', null, title),
        keys ? keysEl(keys) : h('code', { class: 'ks-text' }, text))))));
    if (!groups.size) {
      // Most commands have no shortcut: hand the query to the command palette instead of dead-ending.
      mount(list, h('div', { class: 'empty' },
        h('p', null, 'No shortcuts match'),
        h('button', { class: 'btn', on: { click: searchCommands } }, icon('command', 'sm'), `Search all commands for “${filter.value.trim()}”`, keysEl('Enter'))));
    }
  }
  function searchCommands() {
    const q = filter.value.trim();
    dlg.close();
    execute('palette.commands', q);
  }
  filter.addEventListener('input', render);
  filter.addEventListener('keydown', (e) => { if (e.key === 'Enter' && !list.querySelector('.ks-group')) { e.preventDefault(); searchCommands(); } });
  render();
  const dlg = openDialog({
    title: 'Keyboard shortcuts',
    className: 'wide',
    body: h('div', { class: 'col', style: { gap: '12px' } }, filter, list),
  });
}

// ---------- auth screen ----------
// Shown on 401: this browser has no session cookie for this exact address (the page was
// opened without ferro's token link, or on another host name: 127.0.0.1 and localhost
// are separate sites). Pasting the link or token signs in without a terminal round trip.
export function showAuthScreen(root) {
  const example = `http://${location.host}/?token=…`;
  const input = h('input', { class: 'input mono', type: 'text', placeholder: example, 'aria-label': 'Session link or token', autocomplete: 'off', spellcheck: 'false' });
  const submit = h('button', { class: 'btn primary', type: 'submit' }, 'Continue');
  const err = h('p', { class: 'auth-error', role: 'alert', hidden: true });
  const fail = (msg) => { err.textContent = msg; err.hidden = false; input.focus(); input.select(); };

  async function onSubmit(e) {
    e.preventDefault(); // handled here: the page CSP has form-action 'none'
    err.hidden = true;
    const raw = input.value.trim();
    const token = tokenFromInput(raw);
    if (!token) { fail('That doesn’t look like a ferro link or token.'); return; }
    // A link for another ferro address (other port, localhost vs 127.0.0.1): open this page there.
    let other = null;
    try { const u = new URL(raw); if (/^https?:$/.test(u.protocol) && u.origin !== location.origin) other = u; } catch { /* bare token */ }
    if (other) {
      const next = new URL(location.pathname, other.origin);
      next.searchParams.set('token', token);
      location.assign(next);
      return;
    }
    // Check the token before navigating: a wrong one would land on the server's raw 403.
    submit.disabled = true;
    const res = await fetch(new URL('api/v1/meta', document.baseURI), { headers: { Authorization: `Bearer ${token}` }, credentials: 'omit', cache: 'no-store' }).catch(() => null);
    submit.disabled = false;
    if (!res) { fail('Cannot reach the ferro server at this address.'); return; }
    if (!res.ok) { fail('That token does not match this server. Copy the newest link from the terminal that started ferro.'); return; }
    // The server trades ?token= for an HttpOnly cookie and redirects back to this page.
    const next = new URL(location.href);
    next.searchParams.set('token', token);
    location.replace(next);
  }

  const el = h('div', { class: 'auth-screen' },
    h('div', { class: 'auth-card' },
      brandMark(),
      h('h1', null, 'Sign in to this ferro session'),
      h('p', { class: 'muted' }, 'Paste the link ferro printed when it started, or just its token.'),
      h('form', { class: 'auth-form', on: { submit: onSubmit } }, input, submit),
      err,
      h('details', { class: 'auth-help' },
        h('summary', null, 'Where do I find it?'),
        h('p', { class: 'small muted' }, 'The terminal that started ferro prints it:'),
        h('pre', { class: 'auth-code' }, h('span', { class: 'faint' }, '$ '), 'ferro .\n', h('span', { class: 'faint' }, 'ferro '), example),
        h('p', { class: 'small muted' }, 'A link works for the exact address it names: 127.0.0.1 and localhost are separate sessions. Closing the browser ends the session.')),
      h('p', { class: 'faint small' }, 'The token keeps other websites on this machine from reading your code or running git commands.')));
  root.appendChild(el);
  input.focus();
}

// ---------- inspector: info ----------
export function renderInfoTab(el) {
  const ws = h('dl', { class: 'kv' });
  const idx = h('dl', { class: 'kv' });
  const srv = h('dl', { class: 'kv' });
  const feats = h('div', { class: 'feature-chips' });
  mount(el,
    h('section', { class: 'insp-section' }, h('h3', { class: 'label' }, 'Workspace'), ws),
    h('section', { class: 'insp-section' }, h('h3', { class: 'label' }, 'Index'), idx),
    h('section', { class: 'insp-section' }, h('h3', { class: 'label' }, 'Server'), srv),
    h('section', { class: 'insp-section' }, h('h3', { class: 'label' }, 'Capabilities'), feats));
  const kv = (dl, rows) => mount(dl, rows.flatMap(([k, v]) => [h('dt', null, k), h('dd', null, v ?? '—')]));
  function render() {
    const m = store.get('meta');
    const i = store.get('index');
    const g = store.get('git');
    const met = store.get('metrics');
    kv(ws, [['Name', m?.workspace?.name], ['Root', m?.workspace?.root], ['Branch', g?.branch], ['HEAD', g?.headSha?.slice(0, 10)], ['Mode', m?.mode]]);
    kv(idx, [['State', i?.state], ['Files', i ? formatCount(i.files) : null], ['Walk', i ? formatMs(i.ms) : null], ['Generation', i?.generation], ['Search index', i?.searchIndex]]);
    kv(srv, [['Version', m?.version], ['Host', m?.host], ['API', m ? `v${m.api} · spec ${m.specVersion}` : null], ['Memory', met ? formatBytes(met.rssBytes) : null], ['CPU', met ? `${met.cpuPct}%` : null]]);
    mount(feats, (m?.features || []).map((f) => h('span', { class: 'chip' }, f)));
  }
  for (const k of ['meta', 'index', 'git', 'metrics']) store.subscribe(k, render);
  render();
}

// ---------- inspector: AI (placeholder until backend B5) ----------
export function renderAiTab(el) {
  mount(el,
    h('div', { class: 'ai-intro' },
      h('span', { class: 'a-icon' }, icon('sparkles', 'lg')),
      h('h3', null, 'AI review is coming'),
      h('p', { class: 'muted' }, 'Ask questions about the code, get an AI first pass on a pull request, and accept or dismiss each finding as a draft comment.'),
      h('ul', { class: 'ai-list' },
        h('li', null, icon('message', 'sm'), 'Ask with the current file or selection as context'),
        h('li', null, icon('git-pull-request', 'sm'), 'Ranked findings on the PR diff'),
        h('li', null, icon('check', 'sm'), 'Accept, edit or dismiss — nothing posts without you')),
      h('p', { class: 'faint small' }, `Coming in the next release. It will use a provider key (ANTHROPIC_API_KEY) from where ferro runs; ${isMac ? '⌘' : 'Ctrl+'}I opens it here.`)));
}
