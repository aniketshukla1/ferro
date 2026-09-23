const $ = (id) => document.getElementById(id);
const q = null, filesEl = $('files');
const openBtn = $('open'), askq = $('askq'), themeBtn = $('theme-btn');
const viewport = $('viewport'), spacer = $('spacer'), rowsEl = $('rows'), plainEl = $('plain');
const askpanel = $('askpanel'), askbody = $('askbody'), askclose = $('askclose');
const filebar = $('filebar'), fbPath = $('filebar-path'), fbMeta = $('filebar-meta'), fbDirty = $('filebar-dirty');
const diffview = $('diffview');
const homeEl = $('home'), heroStats = $('hero-stats'), heroKeys = $('hero-keys'), heroRecent = $('hero-recent');
const outlineEl = $('outline'), olItems = $('ol-items');
const pal = $('palette'), palInput = $('palette-input'), palRes = $('palette-results');
const sideCount = $('side-count'), reindexBtn = $('reindex');
let allFiles = [];
let gitMap = new Map(), gitBranch = '';
let currentPath = null;

const tauriInvoke = window?.__TAURI__?.core?.invoke ?? null;
const backend = tauriInvoke ? 'tauri' : 'http';
const invoke = tauriInvoke;

/* ---------- backend ---------- */
async function apiStats() {
  if (invoke) return await invoke('get_stats');
  return await (await fetch('/api/stats')).json();
}
async function apiFiles() {
  if (invoke) return await invoke('list_files');
  return await (await fetch('/api/files')).json();
}
async function apiFuzzy(v) {
  if (invoke) return await invoke('fuzzy', { q: v, limit: 9 });
  return await (await fetch('/api/fuzzy?q=' + encodeURIComponent(v) + '&limit=9')).json();
}
async function apiSearch(v) {
  if (invoke) return await invoke('grep', { q: v, limit: 30 });
  return await (await fetch('/api/search?q=' + encodeURIComponent(v) + '&limit=30')).json();
}
async function apiMeta(path) {
  if (invoke) return await invoke('file_meta', { path });
  return await (await fetch('/api/file-meta?path=' + encodeURIComponent(path))).json();
}
async function apiHighlight(path, start, count) {
  if (invoke) return await invoke('highlight', { path, start, count });
  const r = await fetch(`/api/highlight?path=${encodeURIComponent(path)}&start=${start}&count=${count}`);
  if (!r.ok) throw new Error('hl ' + r.status);
  return await r.json();
}
async function apiWindow(path, start, count) {
  if (invoke) return await invoke('read_window', { path, start, count });
  const r = await fetch(`/api/file-window?path=${encodeURIComponent(path)}&start=${start}&count=${count}`);
  if (!r.ok) throw new Error('win ' + r.status);
  return await r.json();
}
async function apiDiff() {
  if (invoke) return await invoke('git_diff', { path: null });
  return await (await fetch('/api/diff')).text();
}
async function apiGitStatus() {
  if (invoke) return await invoke('git_status');
  return await (await fetch('/api/git-status')).text();
}
async function apiAsk(question) {
  if (invoke) return await invoke('ask', { question, maxSteps: 8 });
  const r = await fetch('/api/ask', { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ question }) });
  if (!r.ok) throw new Error(await r.text());
  return await r.json();
}
async function apiReindex() {
  if (invoke) return await invoke('reindex');
  await bootStats();
}

function esc(s) {
  return String(s).replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
}

/* ---------- themes ---------- */
const THEMES = ['tokyo', 'paper', 'mocha'];
function setTheme(t) {
  document.documentElement.dataset.theme = t;
  try { localStorage.setItem('ferro-theme', t); } catch {}
}
setTheme((() => { try { return localStorage.getItem('ferro-theme') || 'tokyo'; } catch { return 'tokyo'; } })());
themeBtn.onclick = () => {
  const cur = document.documentElement.dataset.theme || 'tokyo';
  setTheme(THEMES[(THEMES.indexOf(cur) + 1) % THEMES.length]);
};

/* ---------- sidebar ---------- */
const EXT_COLORS = { rs:'#ff8c1a', py:'#7aa2f7', js:'#e0af68', ts:'#7aa2f7', tsx:'#7aa2f7', go:'#66d9e8', md:'#9ece6a', json:'#bb9af7', toml:'#8b93a7', css:'#bb9af7', html:'#f7768e', sh:'#9ece6a', sql:'#66d9e8' };
function extOf(p) { const i = p.lastIndexOf('.'); return i < 0 ? '' : p.slice(i + 1).toLowerCase(); }
function renderSidebar(list) {
  const paths = list.slice(0, 6000).map(f => (typeof f === 'string' ? f : f.path)).sort();
  sideCount.textContent = list.length > 6000 ? '6000+' : list.length;
  // Build tree.
  const root = { dirs: new Map(), files: [] };
  for (const p of paths) {
    const parts = p.split('/');
    let node = root;
    for (let i = 0; i < parts.length - 1; i++) {
      if (!node.dirs.has(parts[i])) node.dirs.set(parts[i], { dirs: new Map(), files: [], open: parts.length < 4 });
      node = node.dirs.get(parts[i]);
    }
    node.files.push(parts[parts.length - 1]);
  }
  filesEl.innerHTML = '';
  const frag = document.createDocumentFragment();
  let rendered = 0;
  const CAP = 3000;
  function hasDirty(node, prefix) {
    for (const [k, v] of gitMap) {
      if (k === prefix || k.startsWith(prefix + '/')) return true;
    }
    return false;
  }
  function emitInto(container, node, prefix, depth) {
    const sub = document.createDocumentFragment();
    for (const [name, s] of [...node.dirs.entries()].sort()) {
      if (rendered++ > CAP) break;
      const full = prefix ? prefix + '/' + name : name;
      const d = document.createElement('div');
      d.className = 't-dir';
      d.innerHTML = `<span class="tw">${s.open ? '▾' : '▸'}</span><span>${esc(name)}</span>` +
        (hasDirty(s, full) ? '<span class="ddot"></span>' : '');
      const kids = document.createElement('div');
      kids.className = 't-kids';
      kids.hidden = !s.open;
      d.onclick = () => { s.open = !s.open; kids.hidden = !s.open; d.querySelector('.tw').textContent = s.open ? '▾' : '▸'; };
      sub.appendChild(d); sub.appendChild(kids);
      emitInto(kids, s, full, depth + 1);
    }
    for (const nm of [...node.files].sort()) {
      if (rendered++ > CAP) break;
      sub.appendChild(fileRow(prefix ? prefix + '/' + nm : nm, nm));
    }
    container.appendChild(sub);
  }
  function fileRow(p, nm) {
    const d = document.createElement('div');
    d.className = 'f' + (p === currentPath ? ' current' : '');
    const st = gitMap.get(p) || '';
    d.innerHTML = `<span class="dot" style="background:${EXT_COLORS[extOf(p)] || 'var(--mut)'}"></span>` +
      `<span class="nm" title="${esc(p)}">${esc(nm)}</span>` +
      (st ? `<span class="badge ${esc(st[0])}">${esc(st[0])}</span>` : '');
    d.onclick = () => openFile(p);
    return d;
  }
  emitInto(frag, root, '', 0);
  filesEl.appendChild(frag);
  if (rendered > CAP) {
    const more = document.createElement('div');
    more.className = 't-dir';
    more.textContent = `… ${paths.length - CAP} more — use Ctrl+K`;
    filesEl.appendChild(more);
  }
}

/* ---------- tabs ---------- */
let tabs = [];
const tabsEl = $('tabs');
function renderTabs() {
  tabsEl.hidden = tabs.length === 0;
  tabsEl.innerHTML = '';
  for (const p of tabs) {
    const t = document.createElement('div');
    t.className = 'tab' + (p === currentPath ? ' active' : '');
    const nm = p.split('/').pop();
    const st = gitMap.get(p) || '';
    t.innerHTML = `<span class="dot" style="width:6px;height:6px;border-radius:50%;background:${EXT_COLORS[extOf(p)] || 'var(--mut)'}"></span>` +
      `<span title="${esc(p)}">${esc(nm)}</span>${st ? '<span style="color:var(--yellow)">●</span>' : ''}`;
    const x = document.createElement('button');
    x.className = 'x'; x.textContent = '×';
    x.onclick = (e) => { e.stopPropagation(); closeTab(p); };
    t.onclick = () => openFile(p);
    t.appendChild(x);
    tabsEl.appendChild(t);
  }
}
function closeTab(p) {
  tabs = tabs.filter(t => t !== p);
  renderTabs();
  if (p === currentPath) {
    if (tabs.length) openFile(tabs[tabs.length - 1]);
    else showHome();
  }
}

function parseGitStatus(text) {
  gitMap = new Map(); gitBranch = '';
  for (const line of String(text).split('\n')) {
    if (line.startsWith('## ')) { gitBranch = line.slice(3).split('...')[0]; continue; }
    if (line.length > 3) {
      const xy = line.slice(0, 2).trim() || 'M';
      const p = line.slice(3).trim().replace(/^"(.+)"$/, '$1');
      if (p) gitMap.set(p, xy);
    }
  }
}

/* ---------- status bar ---------- */
function status() {
  $('st-backend').textContent = backend;
  $('st-branch').textContent = gitBranch ? '⎇ ' + gitBranch : '';
  $('st-file').textContent = currentPath || '';
}

/* ---------- virtual viewer ---------- */
const ROW_H = 20, OVERSCAN = 24, WIN = 200;
const cur = { path: null, total: 0, cache: new Map(), mode: 'plain', pending: new Set() };

function showFileMode() { cur.mode = 'file'; plainEl.hidden = true; askpanel.hidden = true; diffview.hidden = true; homeEl.hidden = true; outlineEl.hidden = true; viewport.hidden = false; filebar.hidden = false; }
function showPlainMode(text) {
  cur.mode = 'plain'; viewport.hidden = true; askpanel.hidden = true; diffview.hidden = true; filebar.hidden = true; homeEl.hidden = true; outlineEl.hidden = true; plainEl.hidden = false;
  if (text !== undefined) plainEl.textContent = text;
}
function showHome() {
  cur.mode = 'home'; cur.path = null; currentPath = null;
  viewport.hidden = true; askpanel.hidden = true; diffview.hidden = true; filebar.hidden = true; plainEl.hidden = true; homeEl.hidden = false;
  renderHome(); status(); renderTabs(); renderSidebar(allFiles);
}
function renderHome() {
  const s = lastStats || { files: 0, indexed_ms: 0, root: '' };
  heroStats.innerHTML =
    `<span class="chip"><b>${s.files}</b> files</span>` +
    `<span class="chip">indexed in <b>${s.indexed_ms}ms</b></span>` +
    `<span class="chip">${esc(s.root.split('/').pop() || '')}</span>` +
    `<span class="chip">${esc(backend)}</span>`;
  heroKeys.innerHTML = [
    ['Ctrl+K', 'palette'], ['Ctrl+D', 'diff vs HEAD'], ['Ctrl+Shift+O', 'outline'],
    ['Ctrl+W', 'close tab'], ['Ctrl+O', 'open folder'], ['?', 'help'],
  ].map(([k, v]) => `<div class="krow"><span>${v}</span><kbd>${k}</kbd></div>`).join('');
  heroRecent.innerHTML = tabs.length
    ? tabs.map(p => `<div class="rrow" data-p="${esc(p)}">${esc(p)}</div>`).join('')
    : '<div class="krow"><span>no recent files yet</span></div>';
  heroRecent.querySelectorAll('.rrow').forEach(el => el.onclick = () => openFile(el.dataset.p));
}
document.querySelectorAll('.hero-actions button').forEach(b => b.onclick = () => {
  const act = b.dataset.act;
  if (act === 'palette') openPalette();
  else if (act === 'diff') showDiff();
  else if (act === 'ask') askq.focus();
});
function showDiffMode() {
  cur.mode = 'diff'; viewport.hidden = true; askpanel.hidden = true; plainEl.hidden = true; filebar.hidden = true; homeEl.hidden = true; outlineEl.hidden = true; diffview.hidden = false;
}

/* ---------- symbol outline (regex, zero-config) ---------- */
const SYM_PATTERNS = {
  rs: [[/^\s*(?:pub\s+)?(?:async\s+)?fn\s+([A-Za-z_]\w*)/, 'fn'], [/^\s*(?:pub\s+)?(?:struct|enum|trait|mod)\s+([A-Za-z_]\w*)/, 'ty'], [/^\s*impl(?:\s+[A-Za-z_]\w*)?\s+([A-Za-z_][\w:]*)/, 'impl']],
  py: [[/^\s*(?:async\s+def|def)\s+([A-Za-z_]\w*)/, 'def'], [/^\s*class\s+([A-Za-z_]\w*)/, 'cls']],
  js: [[/^\s*(?:export\s+)?(?:async\s+)?function\s+([A-Za-z_]\w*)/, 'fn'], [/^\s*(?:export\s+)?class\s+([A-Za-z_]\w*)/, 'cls'], [/^\s*(?:export\s+)?(?:const|let)\s+([A-Za-z_]\w*)\s*=\s*(?:\(|async|function)/, 'fn']],
  ts: [[/^\s*(?:export\s+)?(?:async\s+)?function\s+([A-Za-z_]\w*)/, 'fn'], [/^\s*(?:export\s+)?(?:class|interface|type)\s+([A-Za-z_]\w*)/, 'ty'], [/^\s*(?:export\s+)?const\s+([A-Za-z_]\w*)\s*[:=]/, 'var']],
  tsx: [[/^\s*(?:export\s+)?(?:async\s+)?function\s+([A-Za-z_]\w*)/, 'fn'], [/^\s*(?:export\s+)?(?:class|interface|type)\s+([A-Za-z_]\w*)/, 'ty']],
  go: [[/^\s*func\s+(?:\([^)]*\)\s+)?([A-Za-z_]\w*)/, 'fn'], [/^\s*type\s+([A-Za-z_]\w*)/, 'ty']],
  md: [[/^(#{1,3})\s+(.+)$/, 'h']],
};
async function toggleOutline() {
  if (!outlineEl.hidden) { outlineEl.hidden = true; return; }
  if (!cur.path) return;
  outlineEl.hidden = false;
  olItems.innerHTML = '<div class="ol-item">…</div>';
  let text = '';
  try {
    if (invoke) text = await invoke('read_file', { path: cur.path });
    else text = await (await fetch('/api/file?path=' + encodeURIComponent(cur.path))).text();
  } catch { olItems.innerHTML = ''; return; }
  const pats = SYM_PATTERNS[extOf(cur.path)] || [];
  const syms = [];
  text.split('\n').slice(0, 20000).forEach((line, i) => {
    for (const [re, kind] of pats) {
      const m = line.match(re);
      if (m) { syms.push({ n: i + 1, k: kind, name: m[1] || m[2] }); break; }
    }
  });
  olItems.innerHTML = '';
  if (!syms.length) { olItems.innerHTML = '<div class="ol-item">no symbols</div>'; return; }
  for (const s of syms.slice(0, 400)) {
    const d = document.createElement('div');
    d.className = 'ol-item';
    d.innerHTML = `<span class="k">${esc(s.k)}</span><span>${esc(s.name)}</span>`;
    d.onclick = () => { viewport.scrollTop = Math.max(0, (s.n - 8) * ROW_H); paint(); };
    olItems.appendChild(d);
  }
}

/* ---------- unified diff render ---------- */
function parseDiff(text) {
  const files = [];
  let cur_f = null, cur_h = null;
  for (const raw of String(text).split('\n')) {
    if (raw.startsWith('diff --git ')) {
      const m = raw.match(/ b\/(.+)$/);
      cur_f = { file: m ? m[1] : raw, hunks: [] };
      files.push(cur_f); cur_h = null;
    } else if (raw.startsWith('@@ ')) {
      const m = raw.match(/@@ -(\d+)(?:,\d+)? \+(\d+)(?:,\d+)? @@/);
      cur_h = { header: raw, lines: [], o: m ? +m[1] : 0, n: m ? +m[2] : 0 };
      cur_f?.hunks.push(cur_h);
    } else if (cur_h && (raw.startsWith('+') || raw.startsWith('-') || raw.startsWith(' '))) {
      const t = raw[0] === '+' ? 'add' : raw[0] === '-' ? 'del' : 'ctx';
      const line = { t, text: raw.slice(1), o: null, n: null };
      if (t === 'del' || t === 'ctx') line.o = cur_h.o++;
      if (t === 'add' || t === 'ctx') line.n = cur_h.n++;
      cur_h.lines.push(line);
    }
  }
  return files;
}
function renderDiff(text) {
  showDiffMode();
  const files = parseDiff(text);
  diffview.innerHTML = '';
  if (!files.length) { diffview.innerHTML = '<div class="d-empty">Working tree clean — no diff vs HEAD.</div>'; return; }
  const frag = document.createDocumentFragment();
  for (const f of files) {
    const box = document.createElement('div');
    box.className = 'd-file';
    const adds = f.hunks.flatMap(h => h.lines).filter(l => l.t === 'add').length;
    const dels = f.hunks.flatMap(h => h.lines).filter(l => l.t === 'del').length;
    box.innerHTML = `<div class="d-fhead">${esc(f.file)} <span style="color:var(--green)">+${adds}</span> <span style="color:var(--red)">−${dels}</span></div>`;
    for (const h of f.hunks) {
      const hd = document.createElement('div');
      hd.className = 'd-hunk'; hd.textContent = h.header;
      box.appendChild(hd);
      for (const l of h.lines) {
        const d = document.createElement('div');
        d.className = 'd-line d-' + l.t;
        const sgn = l.t === 'add' ? '+' : l.t === 'del' ? '−' : ' ';
        d.innerHTML = `<span class="g">${l.o ?? ''}</span><span class="g">${l.n ?? ''}</span><span class="sgn">${sgn}</span><span>${esc(l.text)}</span>`;
        box.appendChild(d);
      }
    }
    frag.appendChild(box);
  }
  diffview.appendChild(frag);
}
async function showDiff() {
  const t = await apiDiff().catch(e => String(e));
  renderDiff(String(t).slice(0, 500000));
  currentPath = null; status();
}

async function openFile(path) {
  currentPath = path;
  if (!tabs.includes(path)) { tabs.push(path); if (tabs.length > 10) tabs.shift(); }
  renderTabs();
  showFileMode();
  cur.path = path; cur.cache.clear(); cur.pending.clear();
  renderSidebar(allFiles);
  let meta;
  try { meta = await apiMeta(path); }
  catch { showPlainMode('cannot open ' + path); return; }
  cur.total = meta.total_lines || 0;
  spacer.style.height = Math.max(1, cur.total) * ROW_H + 'px';
  viewport.scrollTop = 0;
  fbPath.textContent = path;
  const st = gitMap.get(path);
  fbDirty.hidden = !st;
  fbDirty.title = st ? 'modified (' + st + ')' : '';
  fbMeta.textContent = `${cur.total.toLocaleString()} lines · ${(meta.size / 1024).toFixed(1)} KB`;
  $('st-line').textContent = 'Ln 1';
  status();
  await ensureAround(0);
  paint();
}

async function ensureAround(first) {
  if (!cur.path) return;
  const aligned = Math.floor(Math.max(0, first - OVERSCAN) / 100) * 100;
  if (cur.cache.has(aligned) || cur.pending.has(aligned)) return;
  cur.pending.add(aligned);
  try {
    let win;
    try {
      win = await apiHighlight(cur.path, aligned, WIN);
      if (!win.lines) throw new Error('bad hl');
    } catch {
      const raw = await apiWindow(cur.path, aligned, WIN);
      win = { total: raw.total, start: raw.start, lines: raw.lines.map(l => ({ n: l.n, html: esc(l.text) })) };
    }
    cur.total = win.total || cur.total;
    spacer.style.height = Math.max(1, cur.total) * ROW_H + 'px';
    cur.cache.set(aligned, win);
    if (cur.cache.size > 8) cur.cache.delete(cur.cache.keys().next().value);
  } finally { cur.pending.delete(aligned); }
}

function lineAt(n) {
  for (const win of cur.cache.values())
    for (const l of win.lines)
      if (l.n === n) return l;
  return null;
}

function paint() {
  if (cur.mode !== 'file') return;
  const first = Math.max(1, Math.floor(viewport.scrollTop / ROW_H) + 1);
  const visible = Math.ceil(viewport.clientHeight / ROW_H) + 1;
  const from = Math.max(1, first - OVERSCAN), to = Math.min(cur.total, first + visible + OVERSCAN);
  rowsEl.innerHTML = '';
  const frag = document.createDocumentFragment();
  for (let n = from; n <= to; n++) {
    const d = document.createElement('div');
    d.className = 'row';
    d.style.top = (n - 1) * ROW_H + 'px';
    const l = lineAt(n);
    d.innerHTML = `<span class="ln">${n}</span><span>${l ? l.html : ''}</span>`;
    frag.appendChild(d);
  }
  rowsEl.appendChild(frag);
  $('st-line').textContent = 'Ln ' + first.toLocaleString() + ' / ' + cur.total.toLocaleString();
  ensureAround(first - 1);
}

let scrollRaf = false, paintQueued = false;
viewport.addEventListener('scroll', () => {
  if (scrollRaf) return;
  scrollRaf = true;
  requestAnimationFrame(() => {
    scrollRaf = false; paint();
    setTimeout(() => {
      if (!paintQueued) { paintQueued = true; setTimeout(() => { paintQueued = false; paint(); }, 120); }
    }, 50);
  });
});

/* ---------- palette ---------- */
let palItems = [], palActive = 0;
const COMMANDS = [
  { name: 'search', hint: '>query — content search' },
  { name: 'diff', hint: 'show diff vs HEAD' },
  { name: 'ask', hint: '>ask question — agent' },
  { name: 'reindex', hint: 'rebuild file index' },
  { name: 'theme', hint: 'cycle theme' },
  { name: 'open', hint: 'open folder (Tauri)' },
];
function openPalette(preset) { pal.hidden = false; palInput.value = preset || ''; updatePalette(); setTimeout(() => palInput.focus(), 0); }
function closePalette() { pal.hidden = true; }
$('palette-trigger').onclick = openPalette;
pal.addEventListener('click', (e) => { if (e.target === pal) closePalette(); });

function renderPal(items) {
  palItems = items; palActive = 0;
  palRes.innerHTML = '';
  items.slice(0, 12).forEach((it, i) => {
    const d = document.createElement('div');
    d.className = 'pr' + (i === 0 ? ' active' : '');
    d.innerHTML = `<span class="k">${esc(it.k)}</span><span>${esc(it.label)}</span>${it.sub ? `<span class="s">${esc(it.sub)}</span>` : ''}`;
    d.onclick = () => runPal(it);
    palRes.appendChild(d);
  });
}
function markPalActive() {
  [...palRes.children].forEach((c, i) => c.classList.toggle('active', i === palActive));
  palRes.children[palActive]?.scrollIntoView({ block: 'nearest' });
}

let palDeb = null;
palInput.addEventListener('input', () => {
  clearTimeout(palDeb);
  palDeb = setTimeout(updatePalette, 70);
});
palInput.addEventListener('keydown', (e) => {
  if (e.key === 'ArrowDown') { e.preventDefault(); palActive = Math.min(palItems.length - 1, palActive + 1); markPalActive(); }
  else if (e.key === 'ArrowUp') { e.preventDefault(); palActive = Math.max(0, palActive - 1); markPalActive(); }
  else if (e.key === 'Enter') { const it = palItems[palActive]; if (it) { closePalette(); runPal(it); } }
  else if (e.key === 'Escape') closePalette();
});

async function updatePalette() {
  const v = palInput.value;
  if (v.startsWith(':')) {
    const n = parseInt(v.slice(1), 10);
    renderPal(Number.isFinite(n) && cur.path
      ? [{ k: 'line', label: `${cur.path}:${n}`, sub: 'go to line', go: { line: n } }]
      : []);
    return;
  }
  if (v === '?') {
    renderPal([
      { k: 'keys', label: 'Ctrl+K palette · Ctrl+D diff · Ctrl+O folder · Esc close', go: null },
      ...COMMANDS.map(c => ({ k: 'cmd', label: c.name, sub: c.hint, go: { cmd: c.name } })),
    ]);
    return;
  }
  if (v.startsWith('>')) {
    const query = v.slice(1).trim();
    const cmds = COMMANDS.filter(c => c.name.startsWith(query)).map(c => ({ k: 'cmd', label: c.name, sub: c.hint, go: { cmd: c.name, arg: query.slice(c.name.length).trim() } }));
    if (!query) { renderPal(cmds); return; }
    if (query.startsWith('ask ')) { renderPal([{ k: 'ask', label: query.slice(4), sub: 'ask agent', go: { cmd: 'ask', arg: query.slice(4) } }]); return; }
    const hits = await apiSearch(query).catch(() => []);
    renderPal([...cmds, ...hits.slice(0, 9).map(h => ({
      k: 'grep', label: `${h.path}:${h.line}`, sub: String(h.text).slice(0, 60),
      go: { file: h.path, line: h.line },
    }))]);
    return;
  }
  if (!v.trim()) {
    renderPal(allFiles.slice(0, 9).map(f => {
      const p = typeof f === 'string' ? f : f.path;
      return { k: 'file', label: p, go: { file: p } };
    }));
    return;
  }
  const res = await apiFuzzy(v).catch(() => []);
  renderPal(res.map(r => ({ k: 'file', label: r.path, sub: String(r.score), go: { file: r.path } })));
}

async function runPal(it) {
  const go = it.go || {};
  if (go.file) {
    await openFile(go.file);
    if (go.line) {
      viewport.scrollTop = Math.max(0, (go.line - 10) * ROW_H);
      paint();
    }
  } else if (go.line && cur.path) {
    viewport.scrollTop = Math.max(0, (go.line - 10) * ROW_H);
    paint();
  } else if (go.cmd === 'diff') {
    showDiff();
  } else if (go.cmd === 'ask' || go.cmd === 'search') {
    askq.value = go.cmd === 'ask' ? (go.arg || '') : '>' + (go.arg || '');
    askq.focus();
    if (go.cmd === 'ask' && go.arg) submitAsk(go.arg);
  } else if (go.cmd === 'reindex') {
    await apiReindex(); await bootStats();
  } else if (go.cmd === 'theme') themeBtn.onclick();
  else if (go.cmd === 'open') pickFolder();
}

/* ---------- ask ---------- */
/* ---------- mini markdown ---------- */
const fileSet = () => new Set(allFiles.map(f => (typeof f === 'string' ? f : f.path)));
function linkify(html) {
  const known = fileSet();
  return html.replace(/([A-Za-z0-9_.\/-]+\.[a-z]{1,5})(:(\d+))?/g, (m, p, _c, ln) => {
    const hit = [...known].find(f => f === p || f.endsWith('/' + p));
    if (!hit) return m;
    return `<a href="#" data-open="${esc(hit)}${ln ? ':' + ln : ''}">${esc(m)}</a>`;
  });
}
function md(src) {
  const blocks = [];
  let text = esc(src).replace(/```(\w*)\n([\s\S]*?)(?:```|$)/g, (_, lang, code) => {
    blocks.push(`<pre class="code"${lang ? ` data-lang="${lang}"` : ''}>${code.replace(/\n$/, '')}</pre>`);
    return `\u0000${blocks.length - 1}\u0000`;
  });
  text = text.split('\n').map(line => {
    if (/^\u0000\d+\u0000$/.test(line.trim())) return line.trim();
    if (/^#{1,4} /.test(line)) return `<h4>${line.replace(/^#{1,4} /, '')}</h4>`;
    if (/^[-*] /.test(line)) return `<li>${line.slice(2)}</li>`;
    if (!line.trim()) return '';
    return `<p>${line}</p>`;
  }).join('\n').replace(/((?:<li>.*?<\/li>\n?)+)/g, '<ul>$1</ul>');
  text = text
    .replace(/\*\*([^*]+)\*\*/g, '<strong>$1</strong>')
    .replace(/`([^`\n]+)`/g, '<code>$1</code>')
    .replace(/\u0000(\d+)\u0000/g, (_, i) => blocks[+i]);
  return linkify(text);
}
document.addEventListener('click', async (e) => {
  const a = e.target.closest?.('a[data-open]');
  if (!a) return;
  e.preventDefault();
  const [p, ln] = a.dataset.open.split(':');
  await openFile(p);
  if (ln) { viewport.scrollTop = Math.max(0, (+ln - 10) * ROW_H); paint(); }
});

async function submitAsk(question) {
  askpanel.hidden = false;
  askbody.innerHTML = '<p>thinking…</p>';
  try {
    const res = await apiAsk(question);
    const t = res.transcript;
    let html = `<h3>Answer</h3>${md(t.final_text || '(no answer)')}`;
    (t.steps || []).forEach((s, i) => {
      const calls = (s.calls || []).map(([call, result]) =>
        `<pre>$ ${esc(call.name)} ${esc(JSON.stringify(call.args))}\n${esc(String(result.output).slice(0, 2000))}</pre>`).join('');
      html += `<details><summary>Step ${i + 1}${s.thought ? ' — ' + esc(s.thought).slice(0, 80) : ''}</summary>${calls}</details>`;
    });
    askbody.innerHTML = html;
  } catch (err) {
    askbody.innerHTML = `<p>ask failed: ${esc(err.message || err)}</p><p>Set GEMINI_API_KEY where the server/desktop runs.</p>`;
  }
}
askq.addEventListener('keydown', async (e) => {
  if (e.key !== 'Enter' || !askq.value.trim()) return;
  const v = askq.value.trim();
  askpanel.hidden = false;
  if (v.startsWith('>')) {
    const hits = await apiSearch(v.slice(1).trim()).catch(() => []);
    askbody.innerHTML = '<h3>Search</h3>' + (hits.map(h =>
      `<pre>${esc(h.path)}:${h.line} ${esc(String(h.text).slice(0, 160))}</pre>`).join('') || '<p>no matches</p>');
    return;
  }
  submitAsk(v);
});
askclose.onclick = () => { askpanel.hidden = true; };

/* ---------- misc ---------- */
async function pickFolder() {
  if (!invoke) return;
  const dir = await invoke('pick_folder');
  if (dir) {
    await invoke('set_root', { path: dir });
    await boot(true);
  }
}
if (openBtn) openBtn.onclick = pickFolder;
$('ol-toggle').onclick = toggleOutline;
reindexBtn.onclick = async () => { await apiReindex(); await boot(true); };

document.addEventListener('keydown', (e) => {
  const mod = e.ctrlKey || e.metaKey;
  if (mod && e.key.toLowerCase() === 'k') { e.preventDefault(); pal.hidden ? openPalette() : closePalette(); }
  else if (mod && e.key.toLowerCase() === 'p') { e.preventDefault(); pal.hidden ? openPalette() : closePalette(); }
  else if (e.key === 'Escape' && !pal.hidden) closePalette();
  else if (mod && e.key.toLowerCase() === 'd') { e.preventDefault(); showDiff(); }
  else if (mod && e.shiftKey && e.key.toLowerCase() === 'o') { e.preventDefault(); toggleOutline(); }
  else if (mod && e.key.toLowerCase() === 'o') { e.preventDefault(); pickFolder(); }
  else if (mod && e.key.toLowerCase() === 'w') { e.preventDefault(); if (currentPath) closeTab(currentPath); }
  else if (e.key === '?' && !/INPUT|TEXTAREA/.test(document.activeElement?.tagName || '')) { e.preventDefault(); openPalette('?'); }
});

/* ---------- boot ---------- */
let lastStats = null;
async function bootStats() {
  try {
    const s = await apiStats();
    lastStats = s;
    $('st-backend').textContent = backend;
    $('st-files').textContent = `${s.files} files`;
    $('st-index').textContent = `${s.indexed_ms}ms`;
  } catch {}
}
async function boot(reset) {
  const s = await apiStats().catch(() => ({ files: 0, indexed_ms: 0, root: '' }));
  lastStats = s;
  allFiles = await apiFiles().catch(() => []);
  try {
    const g = await apiGitStatus();
    parseGitStatus(g);
  } catch {}
  renderSidebar(allFiles);
  status();
  setTimeout(bootStats, 800);
  if (!invoke && openBtn) openBtn.style.display = 'none';
  showHome();
}
boot(false);
