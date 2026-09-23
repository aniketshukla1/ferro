const $ = (id) => document.getElementById(id);
const q = null, filesEl = $('files');
const openBtn = $('open'), askq = $('askq'), themeBtn = $('theme-btn');
const viewport = $('viewport'), spacer = $('spacer'), rowsEl = $('rows'), plainEl = $('plain');
const askpanel = $('askpanel'), askbody = $('askbody'), askclose = $('askclose');
const filebar = $('filebar'), fbPath = $('filebar-path'), fbMeta = $('filebar-meta'), fbDirty = $('filebar-dirty');
const diffview = $('diffview');
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
  filesEl.innerHTML = '';
  sideCount.textContent = list.length;
  for (const f of list.slice(0, 400)) {
    const p = (typeof f === 'string') ? f : f.path;
    const d = document.createElement('div');
    d.className = 'f' + (p === currentPath ? ' current' : '');
    const slash = p.lastIndexOf('/');
    const dir = slash < 0 ? '' : p.slice(0, slash);
    const nm = slash < 0 ? p : p.slice(slash + 1);
    const st = gitMap.get(p) || '';
    d.innerHTML = `<span class="dot" style="background:${EXT_COLORS[extOf(p)] || 'var(--mut)'}"></span>` +
      `<span class="nm" title="${esc(p)}">${esc(nm)}</span>` +
      (st ? `<span class="badge ${esc(st[0])}">${esc(st[0])}</span>` : '') +
      (dir ? `<span class="dir">${esc(dir.split('/').pop())}</span>` : '');
    d.onclick = () => openFile(p);
    filesEl.appendChild(d);
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

function showFileMode() { cur.mode = 'file'; plainEl.hidden = true; askpanel.hidden = true; diffview.hidden = true; viewport.hidden = false; filebar.hidden = false; }
function showPlainMode(text) {
  cur.mode = 'plain'; viewport.hidden = true; askpanel.hidden = true; diffview.hidden = true; filebar.hidden = true; plainEl.hidden = false;
  if (text !== undefined) plainEl.textContent = text;
}
function showDiffMode() {
  cur.mode = 'diff'; viewport.hidden = true; askpanel.hidden = true; plainEl.hidden = true; filebar.hidden = true; diffview.hidden = false;
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
function openPalette() { pal.hidden = false; palInput.value = ''; renderPal([]); setTimeout(() => palInput.focus(), 0); }
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
async function submitAsk(question) {
  askpanel.hidden = false;
  askbody.innerHTML = '<p>thinking…</p>';
  try {
    const res = await apiAsk(question);
    const t = res.transcript;
    let html = `<h3>Answer</h3><p>${esc(t.final_text)}</p>`;
    (t.steps || []).forEach((s, i) => {
      html += `<h3>Step ${i + 1}${s.thought ? ' — ' + esc(s.thought) : ''}</h3>`;
      (s.calls || []).forEach(([call, result]) => {
        html += `<pre>$ ${esc(call.name)} ${esc(JSON.stringify(call.args))}\n${esc(String(result.output).slice(0, 2000))}</pre>`;
      });
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
reindexBtn.onclick = async () => { await apiReindex(); await boot(true); };

document.addEventListener('keydown', (e) => {
  const mod = e.ctrlKey || e.metaKey;
  if (mod && e.key.toLowerCase() === 'k') { e.preventDefault(); pal.hidden ? openPalette() : closePalette(); }
  else if (mod && e.key.toLowerCase() === 'p') { e.preventDefault(); pal.hidden ? openPalette() : closePalette(); }
  else if (e.key === 'Escape' && !pal.hidden) closePalette();
  else if (mod && e.key.toLowerCase() === 'd') { e.preventDefault(); showDiff(); }
  else if (mod && e.key.toLowerCase() === 'o') { e.preventDefault(); pickFolder(); }
});

/* ---------- boot ---------- */
async function bootStats() {
  try {
    const s = await apiStats();
    $('st-backend').textContent = backend;
    $('st-files').textContent = `${s.files} files`;
    $('st-index').textContent = `${s.indexed_ms}ms`;
  } catch {}
}
async function boot(reset) {
  const s = await apiStats().catch(() => ({ files: 0, indexed_ms: 0, root: '' }));
  allFiles = await apiFiles().catch(() => []);
  try {
    const g = await apiGitStatus();
    parseGitStatus(g);
  } catch {}
  renderSidebar(allFiles);
  status();
  setTimeout(bootStats, 800);
  if (!invoke && openBtn) openBtn.style.display = 'none';
  if (reset) { currentPath = null; showPlainMode('Select a file, or press Ctrl+K to jump anywhere.'); }
}
boot(false);
