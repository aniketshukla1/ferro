const $ = (id) => document.getElementById(id);
const q = null, filesEl = $('files');
const openBtn = $('open'), askq = $('askq'), themeBtn = $('theme-btn');
const viewport = $('viewport'), spacer = $('spacer'), rowsEl = $('rows'), plainEl = $('plain');
const askpanel = $('askpanel'), askbody = $('askbody'), askclose = $('askclose');
const filebar = $('filebar'), fbPath = $('filebar-path'), fbMeta = $('filebar-meta'), fbDirty = $('filebar-dirty');
const diffview = $('diffview');
const mdview = $('mdview'), imgview = $('imgview'), imgEl = $('img'), imgWrap = $('imgwrap');
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
async function apiMarkdown(path) {
  if (invoke) return await invoke('markdown', { path });
  const r = await fetch('/api/markdown?path=' + encodeURIComponent(path));
  if (!r.ok) throw new Error('md ' + r.status);
  return await r.text();
}
async function apiImageUrl(path) {
  if (invoke) return await invoke('read_image', { path });
  return '/api/raw?path=' + encodeURIComponent(path);
}

async function openImage(path, meta) {
  cur.mode = 'image'; cur.path = path; currentPath = path;
  plainEl.hidden = true; askpanel.hidden = true; diffview.hidden = true; homeEl.hidden = true;
  outlineEl.hidden = true; gitpanel.hidden = true; viewport.hidden = true; mdview.hidden = true;
  filebar.hidden = false; imgview.hidden = false;
  renderTabs(); renderSidebar(allFiles);
  fbPath.textContent = path;
  fbMeta.textContent = `${(meta.size / 1024).toFixed(1)} KB · image`;
  const st = gitMap.get(path);
  fbDirty.hidden = !st;
  $('img-meta').textContent = path;
  imgState = { scale: 1, bg: 0 };
  imgEl.src = await apiImageUrl(path);
  imgEl.onload = () => {
    $('img-zoom').textContent = `${imgEl.naturalWidth}×${imgEl.naturalHeight}`;
    fitImage();
  };
  status();
}
function applyImg() {
  imgEl.style.width = (imgEl.naturalWidth * imgState.scale) + 'px';
  $('img-zoom').textContent = `${imgEl.naturalWidth}×${imgEl.naturalHeight} · ${Math.round(imgState.scale * 100)}%`;
  imgWrap.classList.toggle('light', imgState.bg === 2);
  imgWrap.classList.toggle('dark', imgState.bg === 1);
}
function fitImage() {
  if (!imgEl.naturalWidth) return;
  imgState.scale = Math.min(2, (imgWrap.clientWidth - 40) / imgEl.naturalWidth);
  applyImg();
}
imgEl.addEventListener('load', applyImg);
imgEl.addEventListener('wheel', (e) => {
  if (cur.mode !== 'image') return;
  e.preventDefault();
  imgState.scale = Math.min(32, Math.max(0.05, imgState.scale * (e.deltaY < 0 ? 1.15 : 1 / 1.15)));
  applyImg();
}, { passive: false });
{
  let drag = null;
  imgWrap.addEventListener('mousedown', (e) => { drag = { x: e.clientX, y: e.clientY, l: imgWrap.scrollLeft, t: imgWrap.scrollTop }; });
  addEventListener('mousemove', (e) => {
    if (!drag) return;
    imgWrap.scrollLeft = drag.l - (e.clientX - drag.x);
    imgWrap.scrollTop = drag.t - (e.clientY - drag.y);
  });
  addEventListener('mouseup', () => { drag = null; });
}
async function toggleMd() {
  if (cur.mode !== 'file' && cur.mode !== 'md' || !cur.path || !isMd(cur.path)) return;
  if (cur.mode === 'md') {
    cur.mode = 'file'; mdview.hidden = true; viewport.hidden = false; paint();
    return;
  }
  try {
    mdview.innerHTML = await apiMarkdown(cur.path);
    cur.mode = 'md'; viewport.hidden = true; mdview.hidden = false;
  } catch { $('st-line').textContent = 'preview failed'; }
}
async function apiGitStatus() {
  if (invoke) return await invoke('git_status');
  return await (await fetch('/api/git-status')).text();
}
async function apiAsk(question) {
  if (invoke) return await invoke('ask', { question, maxSteps: askSteps });
  const r = await fetch('/api/ask', { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ question, max_steps: askSteps }) });
  if (!r.ok) throw new Error(await r.text());
  return await r.json();
}
async function apiReindex() {
  if (invoke) return await invoke('reindex');
  await bootStats();
}
async function apiPrInfo() {
  if (invoke) return await invoke('pr_info');
  try { return await (await fetch('/api/pr-info')).json(); }
  catch { return { pr: null }; }
}

function esc(s) {
  return String(s).replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
}

/* Highlight query matches (case-insensitive). Multi-token: every token lights up. */
function queryTokens(q) {
  return String(q || '').toLowerCase().split(/[^a-z0-9]+/).filter(Boolean);
}
function hi(text, query) {
  const t = String(text);
  const toks = [...new Set(queryTokens(query))].sort((a, b) => b.length - a.length);
  if (!toks.length) return esc(t);
  const low = t.toLowerCase();
  const ranges = [];
  for (const tok of toks) {
    let i = 0;
    while ((i = low.indexOf(tok, i)) >= 0) { ranges.push([i, i + tok.length]); i += tok.length; }
  }
  if (!ranges.length) return esc(t);
  ranges.sort((a, b) => a[0] - b[0]);
  const merged = [];
  for (const r of ranges) {
    const last = merged[merged.length - 1];
    if (last && r[0] <= last[1]) last[1] = Math.max(last[1], r[1]);
    else merged.push([...r]);
  }
  let out = '', pos = 0;
  for (const [a, b] of merged) {
    out += esc(t.slice(pos, a)) + '<mark>' + esc(t.slice(a, b)) + '</mark>';
    pos = b;
  }
  return out + esc(t.slice(pos));
}

/* Every clickable row must be keyboard-operable (skill: no div-only controls). */
function activatable(el, fn) {
  el.tabIndex = 0;
  el.setAttribute('role', 'button');
  el.addEventListener('keydown', (e) => {
    if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); fn(); }
  });
  return el;
}

/* ---------- themes ---------- */
const THEMES = ['forge', 'paper', 'mocha', 'nord', 'dracula', 'gruvbox'];
function setTheme(t) {
  if (!THEMES.includes(t)) t = 'forge';
  document.documentElement.dataset.theme = t;
  try { localStorage.setItem('ferro-theme', t); } catch {}
}
setTheme((() => { try { return localStorage.getItem('ferro-theme') || 'forge'; } catch { return 'forge'; } })());
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
      const toggle = () => { s.open = !s.open; kids.hidden = !s.open; d.querySelector('.tw').textContent = s.open ? '▾' : '▸'; };
      d.onclick = toggle;
      activatable(d, toggle);
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
    const open = () => openFile(p);
    d.onclick = open;
    activatable(d, open);
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
let closedStack = [];
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
    x.setAttribute('aria-label', `Close ${nm}`);
    x.onclick = (e) => { e.stopPropagation(); closeTab(p); };
    t.onclick = () => openFile(p);
    activatable(t, () => openFile(p));
    t.appendChild(x);
    tabsEl.appendChild(t);
  }
}
function closeTab(p) {
  tabs = tabs.filter(t => t !== p);
  closedStack.push(p);
  if (closedStack.length > 25) closedStack.shift();
  renderTabs();
  if (p === currentPath) {
    if (tabs.length) openFile(tabs[tabs.length - 1], true);
    else showHome();
  }
}
function reopenTab() {
  const p = closedStack.pop();
  if (p) openFile(p);
}
function cycleTab(dir) {
  if (tabs.length < 2 || !currentPath) return;
  const i = tabs.indexOf(currentPath);
  openFile(tabs[(i + dir + tabs.length) % tabs.length]);
}

/* ---------- nav history ---------- */
let histBack = [], histFwd = [];
function pushHistory(prev) {
  if (prev) { histBack.push(prev); if (histBack.length > 50) histBack.shift(); }
  histFwd = [];
}
function goHistory(dir) {
  const from = dir < 0 ? histBack : histFwd;
  const to = dir < 0 ? histFwd : histBack;
  const dest = from.pop();
  if (!dest) return;
  if (currentPath) to.push(currentPath);
  openFile(dest, true);
}

function parseGitStatus(text) {
  gitMap = new Map(); gitBranch = ''; gitEntries = [];
  for (const line of String(text).split('\n')) {
    if (line.startsWith('## ')) { gitBranch = line.slice(3).split('...')[0]; continue; }
    if (line.length > 3) {
      const x = line[0] === ' ' ? '' : line[0];
      const y = line[1] === ' ' ? '' : line[1];
      const xy = (x + y) || 'M';
      const p = line.slice(3).trim().replace(/^"(.+)"$/, '$1');
      if (p) { gitMap.set(p, xy); gitEntries.push({ path: p, x, y }); }
    }
  }
}

/* ---------- git panel ---------- */
let gitEntries = [];
const gitpanel = $('gitpanel');
async function gitPost(path, body) {
  if (invoke) {
    const cmd = { '/api/git/stage': 'git_stage', '/api/git/unstage': 'git_unstage', '/api/git/commit': 'git_commit', '/api/git/push': 'git_push', '/api/git/pull': 'git_pull' }[path];
    if (path === '/api/git/commit') return await invoke('git_commit', { message: body.message });
    if (path === '/api/git/commit-message') return await invoke('git_commit_message');
    if (cmd) return await invoke(cmd, body?.paths !== undefined ? { paths: body.paths } : {});
    throw new Error('unknown git op');
  }
  const r = await fetch(path, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(body || {}) });
  const t = await r.text();
  if (!r.ok) throw new Error(t);
  return t;
}
function openGitPanel() {
  cur.mode = 'git';
  viewport.hidden = true; askpanel.hidden = true; diffview.hidden = true; filebar.hidden = true; homeEl.hidden = true; plainEl.hidden = true; outlineEl.hidden = true; mdview.hidden = true; imgview.hidden = true;
  gitpanel.hidden = false;
  renderGitPanel();
}
function checkedPaths() {
  return [...document.querySelectorAll('#git-files input:checked')].map(c => c.dataset.p);
}
function renderGitPanel() {
  $('git-branch').textContent = gitBranch ? '⎇ ' + gitBranch : '';
  const host = $('git-files');
  host.innerHTML = '';
  if (!gitEntries.length) {
    host.innerHTML = '<div class="d-empty">Working tree clean.</div>';
  }
  for (const e of gitEntries) {
    const d = document.createElement('div');
    d.className = 'gf';
    const tag = e.x ? '<span class="staged-tag">staged</span>' : '';
    d.innerHTML = `<input type="checkbox" data-p="${esc(e.path)}" ${e.y ? 'checked' : ''} aria-label="Select ${esc(e.path)}"/>` +
      `<span class="xy">${esc((e.x || ' ') + (e.y || ' '))}</span>` +
      `<span class="nm">${esc(e.path)}</span>${tag}`;
    d.onclick = (ev) => { if (ev.target.tagName !== 'INPUT') { const c = d.querySelector('input'); c.checked = !c.checked; } };
    host.appendChild(d);
  }
}
async function refreshGit() {
  try { parseGitStatus(await apiGitStatus()); } catch {}
  renderSidebar(allFiles);
  status();
  if (cur.mode === 'git') renderGitPanel();
  if (cur.mode === 'file') { const st = gitMap.get(cur.path); fbDirty.hidden = !st; }
}
$('git-stage').onclick = async () => {
  try { $('git-out').textContent = 'staging…'; await gitPost('/api/git/stage', { paths: checkedPaths() }); $('git-out').textContent = 'staged'; }
  catch (e) { $('git-out').textContent = String(e.message || e); }
  refreshGit();
};
$('git-unstage').onclick = async () => {
  try { await gitPost('/api/git/unstage', { paths: checkedPaths() }); $('git-out').textContent = 'unstaged'; }
  catch (e) { $('git-out').textContent = String(e.message || e); }
  refreshGit();
};
$('git-commit-btn').onclick = async () => {
  try {
    const out = await gitPost('/api/git/commit', { message: $('git-msg').value });
    $('git-out').textContent = String(out).split('\n')[0] || 'committed';
    $('git-msg').value = '';
  } catch (e) { $('git-out').textContent = String(e.message || e); }
  refreshGit();
};
$('git-ai').onclick = async () => {
  try {
    $('git-out').textContent = 'drafting…';
    const m = await gitPost('/api/git/commit-message', {});
    const msg = typeof m === 'string' ? JSON.parse(m).message : m.message;
    $('git-msg').value = msg || '';
    $('git-out').textContent = msg ? 'drafted — edit then Commit' : 'no message';
  } catch (e) { $('git-out').textContent = String(e.message || e); }
};
$('git-push').onclick = async () => {
  try { $('git-out').textContent = 'pushing…'; await gitPost('/api/git/push', {}); $('git-out').textContent = 'pushed'; }
  catch (e) { $('git-out').textContent = String(e.message || e); }
  refreshGit();
};
$('git-pull').onclick = async () => {
  try { await gitPost('/api/git/pull', {}); $('git-out').textContent = 'pulled (ff-only)'; }
  catch (e) { $('git-out').textContent = String(e.message || e); }
  refreshGit(); bootStats();
};

/* ---------- status bar ---------- */
function status() {
  $('st-backend').textContent = backend;
  $('st-branch').textContent = gitBranch ? '⎇ ' + gitBranch : '';
  $('st-file').textContent = currentPath || '';
}

/* ---------- virtual viewer ---------- */
const ROW_H = 20, OVERSCAN = 24, WIN = 200;
const cur = { path: null, total: 0, cache: new Map(), mode: 'plain', pending: new Set() };

const IMAGE_EXTS = ['png', 'jpg', 'jpeg', 'gif', 'webp', 'svg', 'ico', 'bmp'];
const isImage = (p) => IMAGE_EXTS.includes(extOf(p || ''));
const isMd = (p) => ['md', 'markdown'].includes(extOf(p || ''));
let mdPreview = false;
let imgState = { scale: 1, bg: 0 };

function showFileMode() { cur.mode = 'file'; plainEl.hidden = true; askpanel.hidden = true; diffview.hidden = true; homeEl.hidden = true; outlineEl.hidden = true; gitpanel.hidden = true; mdview.hidden = true; imgview.hidden = true; viewport.hidden = false; filebar.hidden = false; }
function showPlainMode(text) {
  cur.mode = 'plain'; viewport.hidden = true; askpanel.hidden = true; diffview.hidden = true; filebar.hidden = true; homeEl.hidden = true; outlineEl.hidden = true; gitpanel.hidden = true; mdview.hidden = true; imgview.hidden = true; plainEl.hidden = false;
  if (text !== undefined) plainEl.textContent = text;
}
function showHome() {
  cur.mode = 'home'; cur.path = null; currentPath = null;
  viewport.hidden = true; askpanel.hidden = true; diffview.hidden = true; filebar.hidden = true; plainEl.hidden = true; homeEl.hidden = false; gitpanel.hidden = true; outlineEl.hidden = true; mdview.hidden = true; imgview.hidden = true;
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
  heroRecent.querySelectorAll('.rrow').forEach(el => {
    const open = () => openFile(el.dataset.p);
    el.onclick = open;
    activatable(el, open);
  });
}
document.querySelectorAll('.hero-actions button').forEach(b => b.onclick = () => {
  const act = b.dataset.act;
  if (act === 'palette') openPalette();
  else if (act === 'diff') showDiff();
  else if (act === 'ask') askq.focus();
});
function showDiffMode() {
  cur.mode = 'diff'; viewport.hidden = true; askpanel.hidden = true; plainEl.hidden = true; filebar.hidden = true; homeEl.hidden = true; outlineEl.hidden = true; gitpanel.hidden = true; mdview.hidden = true; imgview.hidden = true; diffview.hidden = false;
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
    const jump = () => { viewport.scrollTop = Math.max(0, (s.n - 8) * ROW_H); paint(); };
    d.onclick = jump;
    activatable(d, jump);
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

async function openFile(path, fromHistory) {
  if (currentPath && currentPath !== path && !fromHistory) pushHistory(currentPath);
  mdPreview = false;
  currentPath = path;
  if (!tabs.includes(path)) { tabs.push(path); if (tabs.length > 10) tabs.shift(); }
  renderTabs();
  if (isImage(path)) {
    let meta = { size: 0 };
    try { meta = await apiMeta(path); } catch {}
    await openImage(path, meta.size ? meta : { size: 0 });
    return;
  }
  showFileMode();
  cur.path = path; cur.cache.clear(); cur.pending.clear();
  wrapCache = null;
  viewport.classList.remove('static');
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
  fbMeta.textContent = `${cur.total.toLocaleString()} lines · ${(meta.size / 1024).toFixed(1)} KB${isMd(path) ? ' · Alt+M preview' : ''}`;
  $('st-line').textContent = 'Ln 1';
  status();
  // Skeleton rows (skill: loading state, not blank) until the first window lands.
  rowsEl.innerHTML = '';
  {
    const frag = document.createDocumentFragment();
    const n = Math.max(8, Math.ceil(viewport.clientHeight / ROW_H) || 20);
    for (let i = 1; i <= Math.min(n, 40); i++) {
      const d = document.createElement('div');
      d.className = 'row skel';
      d.style.top = ((i - 1) * ROW_H) + 'px';
      d.setAttribute('aria-hidden', 'true');
      d.innerHTML = `<span class="ln">${i}</span><span style="width:${55 + ((i * 37) % 35)}%"></span>`;
      frag.appendChild(d);
    }
    rowsEl.appendChild(frag);
  }
  viewport.setAttribute('aria-busy', 'true');
  await ensureAround(0);
  viewport.removeAttribute('aria-busy');
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

function focusRow(n) {
  requestAnimationFrame(() => {
    const rows = rowsEl.children;
    for (const r of rows) {
      const ln = r.querySelector('.ln');
      if (ln && +ln.textContent === n) { r.focus({ preventScroll: true }); break; }
    }
  });
}

function paint() {
  if (cur.mode !== 'file') return;
  if (document.body.classList.contains('wrap') && cur.total <= 2000 && wrapCache) {
    renderWrapped();
    return;
  }
  const first = Math.max(1, Math.floor(viewport.scrollTop / ROW_H) + 1);
  const visible = Math.ceil(viewport.clientHeight / ROW_H) + 1;
  const from = Math.max(1, first - OVERSCAN), to = Math.min(cur.total, first + visible + OVERSCAN);
  rowsEl.innerHTML = '';
  const frag = document.createDocumentFragment();
  for (let n = from; n <= to; n++) {
    const d = document.createElement('div');
    const inSel = cur.path === selectedFile && n >= selStart && n <= selEnd && selStart > 0;
    d.className = 'row' + (n === selectedLine && cur.path === selectedFile ? ' cur-line' : '') + (inSel && n !== selectedLine ? ' in-sel' : '');
    d.style.top = (n - 1) * ROW_H + 'px';
    d.tabIndex = -1;
    const l = lineAt(n);
    const hasDraft = (draftLines.get(cur.path) || []).some(x => x.line === n);
    d.innerHTML = `<span class="ln">${n}</span><span>${l ? l.html : ''}</span>${hasDraft ? '<span class="dmark">◆</span>' : ''}`;
    d.onclick = (e) => {
      if (e.shiftKey && cur.path === selectedFile && selAnchor > 0) setSelection(cur.path, selAnchor, n);
      else setSelection(cur.path, n, n);
      paint();
    };
    d.onkeydown = (e) => {
      if (e.key !== 'ArrowDown' && e.key !== 'ArrowUp') return;
      e.preventDefault();
      const step = e.key === 'ArrowDown' ? 1 : -1;
      const next = Math.min(cur.total, Math.max(1, n + step));
      if (e.shiftKey) setSelection(cur.path, selAnchor || n, next);
      else setSelection(cur.path, next, next);
      ensureAround(next - 1).then(paint);
      viewport.scrollTop = Math.max(0, (next - 6) * ROW_H);
      paint();
      focusRow(next);
    };
    d.oncontextmenu = (e) => { e.preventDefault(); setSelection(cur.path, n, n); paint(); openCtx(e.clientX, e.clientY); };
    frag.appendChild(d);
  }
  rowsEl.appendChild(frag);
  $('st-line').textContent = 'Ln ' + first.toLocaleString() + ' / ' + cur.total.toLocaleString();
  ensureAround(first - 1);
}

/* ---------- selection, wrap, find ---------- */
let selectedFile = null, selectedLine = 0;
let selAnchor = 0, selStart = 0, selEnd = 0;
let wrapCache = null;

function setSelection(file, anchor, focus) {
  selectedFile = file;
  selAnchor = anchor;
  const a = Math.min(anchor, focus), b = Math.max(anchor, focus);
  selStart = a; selEnd = b;
  selectedLine = focus;
  const total = selEnd - selStart + 1;
  $('st-line').textContent = total > 1
    ? `${file.split('/').pop()}:${selStart}-${selEnd} (${total} lines)`
    : `Ln ${focus.toLocaleString()} / ${cur.total.toLocaleString()}`;
}
function selRef() {
  if (!selectedFile || !selStart) return null;
  return selStart === selEnd
    ? `@${selectedFile}:${selStart}`
    : `@${selectedFile}:${selStart}-${selEnd}`;
}
async function copyRef() {
  const ref = selRef();
  if (!ref) return;
  try { await navigator.clipboard.writeText(ref); } catch {}
  $('st-line').textContent = `copied ${ref}`;
}
async function copyWithContext() {
  if (!selectedFile || !selStart) return;
  let win;
  try {
    if (invoke) win = await invoke('read_window', { path: selectedFile, start: selStart - 1, count: selEnd - selStart + 1 });
    else win = await (await fetch(`/api/file-window?path=${encodeURIComponent(selectedFile)}&start=${selStart - 1}&count=${selEnd - selStart + 1}`)).json();
  } catch { return; }
  const body = win.lines.map(l => l.text).join('\n');
  const ref = selRef();
  try { await navigator.clipboard.writeText(`${ref}\n\`\`\`\n${body}\n\`\`\``); } catch {}
  $('st-line').textContent = `copied ${ref} + context`;
}
async function findUsages() {
  if (!selectedFile || !selStart) return;
  let win;
  try {
    if (invoke) win = await invoke('read_window', { path: selectedFile, start: selStart - 1, count: 1 });
    else win = await (await fetch(`/api/file-window?path=${encodeURIComponent(selectedFile)}&start=${selStart - 1}&count=1`)).json();
  } catch { return; }
  const line = win.lines[0]?.text || '';
  const m = line.match(/[A-Za-z_]\w*/);
  if (!m) return;
  openPalette('>' + m[0]);
}

async function renderWrapped() {
  if (!wrapCache) {
    try {
      const t = await apiFile(cur.path);
      wrapCache = String(t).split('\n');
    } catch { wrapCache = []; }
  }
  viewport.classList.add('static');
  rowsEl.innerHTML = '';
  const frag = document.createDocumentFragment();
  wrapCache.slice(0, 2000).forEach((text, i) => {
    const n = i + 1;
    const d = document.createElement('div');
    const inSel = cur.path === selectedFile && n >= selStart && n <= selEnd && selStart > 0;
    d.className = 'row' + (n === selectedLine && cur.path === selectedFile ? ' cur-line' : '') + (inSel && n !== selectedLine ? ' in-sel' : '');
    d.innerHTML = `<span class="ln">${n}</span><span>${esc(text)}</span>`;
    d.onclick = (e) => {
      if (e.shiftKey && cur.path === selectedFile && selAnchor > 0) setSelection(cur.path, selAnchor, n);
      else setSelection(cur.path, n, n);
      renderWrapped();
    };
    frag.appendChild(d);
  });
  rowsEl.appendChild(frag);
}
function toggleWrap() {
  document.body.classList.toggle('wrap');
  wrapCache = null;
  viewport.classList.remove('static');
  if (!document.body.classList.contains('wrap')) { paint(); return; }
  if (cur.mode !== 'file') return;
  if (cur.total > 2000) {
    document.body.classList.remove('wrap');
    $('st-line').textContent = 'wrap off for large files (>2000 lines)';
    return;
  }
  renderWrapped();
}

const findbar = $('findbar'), findInput = $('find-input'), findCount = $('find-count');
let findLines = [], findIx = -1;
async function openFind() {
  if (cur.mode !== 'file' || !cur.path) return;
  findbar.hidden = false;
  findInput.value = '';
  findCount.textContent = '';
  findLines = []; findIx = -1;
  setTimeout(() => findInput.focus(), 0);
}
async function runFind() {
  const query = findInput.value;
  findLines = []; findIx = -1;
  if (!query) { findCount.textContent = ''; paint(); return; }
  let text = '';
  try {
    if (invoke) text = await invoke('read_file', { path: cur.path });
    else text = await (await fetch('/api/file?path=' + encodeURIComponent(cur.path))).text();
  } catch { findCount.textContent = 'err'; return; }
  const ql = query.toLowerCase();
  text.split('\n').forEach((line, i) => {
    if (line.toLowerCase().includes(ql)) findLines.push(i + 1);
  });
  findCount.textContent = findLines.length ? `1/${findLines.length}` : '0';
  if (findLines.length) {
    findIx = 0;
    jumpFind();
  }
}
function jumpFind() {
  if (!findLines.length) return;
  const n = findLines[findIx];
  findCount.textContent = `${findIx + 1}/${findLines.length}`;
  setSelection(cur.path, n, n);
  ensureAround(n - 1).then(paint);
  viewport.scrollTop = Math.max(0, (n - 6) * ROW_H);
  paint();
}
findInput.addEventListener('input', () => { clearTimeout(findInput._d); findInput._d = setTimeout(runFind, 120); });
findInput.addEventListener('keydown', (e) => {
  if (e.key === 'Enter') { e.preventDefault(); if (findLines.length) { findIx = (findIx + (e.shiftKey ? -1 : 1) + findLines.length) % findLines.length; jumpFind(); } }
  else if (e.key === 'Escape') { findbar.hidden = true; selectedLine = 0; paint(); }
});

/* ---------- settings (Ctrl+,) ---------- */
async function apiSettingsGet() {
  if (invoke) return await invoke('get_settings');
  return await (await fetch('/api/settings')).json();
}
async function apiSettingsSave(patch) {
  if (invoke) return await invoke('save_settings', { patch });
  const r = await fetch('/api/settings', { method: 'PUT', headers: { 'content-type': 'application/json' }, body: JSON.stringify(patch) });
  const t = await r.text();
  if (!r.ok) throw new Error(t);
  return JSON.parse(t);
}
function applySettings(s) {
  if (s.theme && !localStorage.getItem('ferro-theme')) setTheme(s.theme);
  document.body.classList.toggle('no-side', s.sidebar === false);
  if (s.wordWrap && !document.body.classList.contains('wrap')) toggleWrap();
  if (!s.wordWrap && document.body.classList.contains('wrap')) toggleWrap();
}
async function openSettings() {
  const panel = $('settingspanel');
  panel.hidden = false;
  let s = {};
  try { s = await apiSettingsGet(); } catch {}
  const sel = $('set-theme');
  sel.innerHTML = '';
  for (const t of THEMES) {
    const o = document.createElement('option');
    o.value = t; o.textContent = t;
    if (s.theme === t) o.selected = true;
    sel.appendChild(o);
  }
  $('set-wrap').checked = !!s.wordWrap;
  $('set-sidebar').checked = s.sidebar !== false;
  $('set-steps').value = s.askMaxSteps ?? 8;
  $('set-raw').value = JSON.stringify(s, null, 2);
  $('set-out').textContent = '';
}
$('set-close').onclick = () => { $('settingspanel').hidden = true; };
$('set-save').onclick = async () => {
  try {
    const patch = JSON.parse($('set-raw').value);
    patch.theme = $('set-theme').value;
    patch.wordWrap = $('set-wrap').checked;
    patch.sidebar = $('set-sidebar').checked;
    patch.askMaxSteps = +$('set-steps').value || 8;
    const eff = await apiSettingsSave(patch);
    localStorage.setItem('ferro-theme', eff.theme || 'forge');
    applySettings(eff);
    $('set-out').textContent = 'saved';
    setTimeout(() => { $('settingspanel').hidden = true; }, 400);
  } catch (e) { $('set-out').textContent = 'error: ' + (e.message || e); }
};

/* ---------- review drafts (Alt+R) ---------- */
const composeEl = $('compose'), composeText = $('compose-text'), composeTitle = $('compose-title');
let prInfo = null;
let draftLines = new Map();

async function apiDrafts() {
  if (invoke) return await invoke('draft_list');
  return await (await fetch('/api/review/drafts')).json();
}
async function refreshDrafts() {
  let all = [];
  try { all = await apiDrafts(); } catch { all = []; }
  draftLines = new Map();
  for (const d of all) {
    if (!draftLines.has(d.path)) draftLines.set(d.path, []);
    draftLines.get(d.path).push(d);
  }
  const n = (draftLines.get(cur.path) || []).length;
  const badge = $('draft-count');
  badge.hidden = cur.mode !== 'file' || n === 0;
  badge.textContent = n ? `${n} draft${n > 1 ? 's' : ''}` : '';
  if (cur.mode === 'file') paint();
  return all;
}
function openCompose() {
  if (!prInfo) { $('st-line').textContent = 'drafts need PR mode: ferro <pr-url>'; return; }
  if (cur.mode !== 'file' || !cur.path || !selStart) { $('st-line').textContent = 'select a line first'; return; }
  composeTitle.textContent = `Comment on ${cur.path}:${selStart}${selEnd !== selStart ? '-' + selEnd : ''}`;
  composeText.value = '';
  composeEl.hidden = false;
  setTimeout(() => composeText.focus(), 0);
}
async function saveCompose() {
  const body = composeText.value.trim();
  if (!body) return;
  const payload = { path: cur.path, line: selStart, body };
  try {
    if (invoke) await invoke('draft_add', payload);
    else {
      const r = await fetch('/api/review/drafts', { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(payload) });
      if (!r.ok) throw new Error(await r.text());
    }
    composeEl.hidden = true;
    await refreshDrafts();
    $('st-line').textContent = 'draft saved';
  } catch (err) {
    $('st-line').textContent = 'draft failed: ' + (err.message || err);
  }
}
$('compose-save').onclick = saveCompose;
composeText.addEventListener('keydown', (e) => {
  if ((e.ctrlKey || e.metaKey) && e.key === 'Enter') { e.preventDefault(); saveCompose(); }
  else if (e.key === 'Escape') composeEl.hidden = true;
});
async function submitReview(event) {
  askpanel.hidden = false;
  askbody.innerHTML = '<p>submitting…</p>';
  try {
    let out;
    if (invoke) out = await invoke('review_submit', { event, body: '' });
    else {
      const r = await fetch('/api/review/submit', { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ event, body: '' }) });
      const t = await r.text();
      if (!r.ok) throw new Error(t);
      out = t;
    }
    askbody.innerHTML = `<h2>Review submitted (${esc(event)})</h2><pre>${esc(String(out).slice(0, 2000))}</pre>`;
    await refreshDrafts();
  } catch (err) {
    askbody.innerHTML = `<p>submit failed: ${esc(err.message || err)}</p>`;
  }
}
async function listDrafts() {
  const all = await refreshDrafts();
  askpanel.hidden = false;
  if (!all.length) { askbody.innerHTML = '<p>no drafts yet — Alt+R on a line</p>'; return; }
  askbody.innerHTML = '<h2>Drafts</h2>' + all.map(d =>
    `<pre data-draft="${esc(d.id)}">${esc(d.path)}:${d.line} — ${esc(d.body)}  [× ${esc(d.id)}]</pre>`).join('') +
    '<p><button id="draft-apply">Batch apply with agent</button></p>' +
    '<p>Delete: click a draft, or submit below.</p>' +
    ['comment', 'approve', 'request-changes'].map(e => `<button data-submit="${e}">Submit ${e}</button> `).join('');
  askbody.querySelectorAll('button[data-submit]').forEach(b => b.onclick = () => submitReview(b.dataset.submit));
  const applyBtn = askbody.querySelector('#draft-apply');
  if (applyBtn) applyBtn.onclick = async () => {
    askbody.innerHTML = '<p>agent applying drafts…</p>';
    try {
      let res;
      if (invoke) res = await invoke('review_apply');
      else {
        const r = await fetch('/api/review/apply', { method: 'POST' });
        const t = await r.text();
        if (!r.ok) throw new Error(t);
        res = JSON.parse(t);
      }
      const t = res.transcript;
      askbody.innerHTML = `<h2>Applied ${res.applied} fix${res.applied === 1 ? '' : 'es'}</h2><p>${esc(t.final_text || '')}</p>`;
      await refreshDrafts();
      const s = await apiStats().catch(() => null);
      if (s) { lastStats = s; }
      allFiles = await apiFiles().catch(() => []);
      renderSidebar(allFiles);
      status();
    } catch (err) {
      askbody.innerHTML = `<p>apply failed: ${esc(err.message || err)}</p>`;
    }
  };
  askbody.querySelectorAll('pre[data-draft]').forEach(p => p.onclick = async () => {
    const id = p.dataset.draft;
    if (invoke) await invoke('draft_delete', { id });
    else await fetch('/api/review/drafts/' + encodeURIComponent(id), { method: 'DELETE' });
    listDrafts();
  });
}
const ctxmenu = $('ctxmenu');
function openCtx(x, y) {
  ctxmenu.innerHTML = '';
  const items = [
    ['Copy ref', 'Alt+C', copyRef],
    ['Copy with context', 'Alt+A', copyWithContext],
    ['Find usages', 'Alt+U', findUsages],
    ['Ask about selection', '', () => {
      const ref = selRef();
      if (ref) { askq.value = `Explain ${ref}`; askq.focus(); }
    }],
  ];
  for (const [label, key, fn] of items) {
    const b = document.createElement('button');
    b.innerHTML = `${esc(label)}${key ? ` <kbd>${esc(key)}</kbd>` : ''}`;
    b.onclick = () => { ctxmenu.hidden = true; fn(); };
    ctxmenu.appendChild(b);
  }
  ctxmenu.hidden = false;
  ctxmenu.style.left = Math.min(x, innerWidth - 220) + 'px';
  ctxmenu.style.top = Math.min(y, innerHeight - 180) + 'px';
}
document.addEventListener('click', (e) => {
  if (!ctxmenu.hidden && !e.target.closest?.('#ctxmenu')) ctxmenu.hidden = true;
});

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
  { name: 'git', hint: 'open git panel' },
  { name: 'settings', hint: 'open settings' },
  { name: 'ask', hint: '>ask question — agent' },
  { name: 'reindex', hint: 'rebuild file index' },
  { name: 'theme', hint: 'cycle theme' },
  { name: 'open', hint: 'open folder (Tauri)' },
];
function openPalette(preset) { pal.hidden = false; palInput.value = typeof preset === 'string' ? preset : ''; updatePalette(); setTimeout(() => palInput.focus(), 0); }
function closePalette() { pal.hidden = true; }
$('palette-trigger').onclick = () => openPalette();
pal.addEventListener('click', (e) => { if (e.target === pal) closePalette(); });

function renderPal(items) {
  palItems = items;
  palActive = items.findIndex(it => !it.header);
  if (palActive < 0) palActive = 0;
  palRes.innerHTML = '';
  items.slice(0, 14).forEach((it) => {
    if (it.header) {
      const h = document.createElement('div');
      h.className = 'pr-head';
      h.textContent = it.label;
      palRes.appendChild(h);
      return;
    }
    const d = document.createElement('div');
    d.className = 'pr' + (palItems.indexOf(it) === palActive ? ' active' : '');
    d.innerHTML = `<span class="k">${esc(it.k)}</span><span>${esc(it.label)}</span>${it.sub ? `<span class="s">${it.html ? it.sub : esc(it.sub)}</span>` : ''}`;
    d.onclick = () => { closePalette(); runPal(it); };
    activatable(d, () => { closePalette(); runPal(it); });
    palRes.appendChild(d);
  });
  markPalActive();
}
function stepPal(dir) {
  if (!palItems.length) return;
  let i = palActive;
  for (let n = 0; n < palItems.length; n++) {
    i = (i + dir + palItems.length) % palItems.length;
    if (!palItems[i].header) break;
  }
  palActive = i;
  markPalActive();
}
function markPalActive() {
  const rows = [...palRes.children].filter(c => !c.classList.contains('pr-head'));
  const sel = palItems[palActive];
  [...palRes.children].forEach((c) => c.classList.remove('active'));
  const ix = palItems.slice(0, 14).filter(i => !i.header).indexOf(sel);
  if (rows[ix]) {
    rows[ix].classList.add('active');
    rows[ix].scrollIntoView({ block: 'nearest' });
  }
}

let palDeb = null;
palInput.addEventListener('input', () => {
  clearTimeout(palDeb);
  palDeb = setTimeout(updatePalette, 70);
});
palInput.addEventListener('keydown', (e) => {
  if (e.key === 'ArrowDown') { e.preventDefault(); stepPal(1); }
  else if (e.key === 'ArrowUp') { e.preventDefault(); stepPal(-1); }
  else if (e.key === 'Enter') { const it = palItems[palActive]; if (it && !it.header) { closePalette(); runPal(it); } }
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
    if (/github\.com\/.+\/pull\/\d+/.test(query)) {
      cmds.unshift({ k: 'pr', label: query, sub: 'open PR review', go: { cmd: 'openpr', arg: query } });
    }
    if (prInfo && 'drafts'.startsWith(query)) cmds.unshift({ k: 'cmd', label: 'drafts', sub: 'list review drafts', go: { cmd: 'drafts' } });
    if (prInfo && 'submit'.startsWith(query)) {
      for (const ev of ['comment', 'approve', 'request-changes']) {
        cmds.unshift({ k: 'cmd', label: `submit ${ev}`, sub: 'post review to GitHub', go: { cmd: 'submit', arg: ev } });
      }
    }
    if (!query) { renderPal(cmds); return; }
    if (query.startsWith('ask ')) { renderPal([{ k: 'ask', label: query.slice(4), sub: 'ask agent', go: { cmd: 'ask', arg: query.slice(4) } }]); return; }
    const hits = await apiSearch(query).catch(() => []);
    renderPal([...cmds, ...hits.slice(0, 9).map(h => ({
      k: 'grep', label: `${h.path}:${h.line}`, html: true,
      sub: hi(String(h.text).slice(0, 120), query),
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
  // Blended: filenames first, then content hits (no > prefix needed).
  const [fuzzyRes, contentHits] = await Promise.all([
    apiFuzzy(v).catch(() => []),
    v.trim().length >= 3 ? apiSearch(v).catch(() => []) : Promise.resolve([]),
  ]);
  const items = fuzzyRes.map(r => ({ k: 'file', label: r.path, sub: String(r.score), go: { file: r.path } }));
  if (contentHits.length) {
    items.push({ header: true, label: `Content — ${contentHits.length} hit${contentHits.length > 1 ? 's' : ''}` });
    for (const h of contentHits.slice(0, 8)) {
      items.push({
        k: 'grep', label: `${h.path}:${h.line}`, html: true,
        sub: hi(String(h.text).slice(0, 120), v),
        go: { file: h.path, line: h.line },
      });
    }
  }
  renderPal(items);
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
  } else if (go.cmd === 'git') {
    openGitPanel();
  } else if (go.cmd === 'openpr') {
    await openPr(go.arg);
  } else if (go.cmd === 'settings') {
    openSettings();
  } else if (go.cmd === 'drafts') {
    listDrafts();
  } else if (go.cmd === 'submit') {
    submitReview(go.arg || 'comment');
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
  // Tauri invoke has no streaming: single JSON round-trip.
  if (invoke) {
    try {
      const res = await apiAsk(question);
      renderTranscript(res.transcript);
    } catch (err) {
      askbody.innerHTML = `<p>ask failed: ${esc(err.message || err)}</p><p>Set GEMINI_API_KEY where the desktop runs.</p>`;
    }
    return;
  }
  // Browser: step-level SSE stream with JSON fallback.
  try {
    const r = await fetch('/api/ask/stream', { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ question, max_steps: askSteps }) });
    if (!r.ok || !r.body) throw new Error(await r.text());
    const reader = r.body.getReader();
    const dec = new TextDecoder();
    let buf = '', steps = [], cur = null, finalText = '';
    askbody.innerHTML = '<h2>Answer</h2><div id="live"></div>';
    const live = () => askbody.querySelector('#live');
    let liveAnswer = '';
    const paintLive = () => {
      let html = liveAnswer ? `<p class="streaming">${esc(liveAnswer)}▍</p>` : '';
      steps.forEach((s, i) => {
        html += `<details open><summary>Step ${i + 1}${s.thought ? ' — ' + esc(s.thought).slice(0, 80) : ''}</summary>` +
          s.calls.map(([n, a, o]) => `<pre>$ ${esc(n)} ${esc(JSON.stringify(a))}\n${esc(String(o).slice(0, 1500))}</pre>`).join('') + '</details>';
      });
      if (cur) html += `<details open><summary>Step ${steps.length + 1}${cur.thought ? ' — ' + esc(cur.thought).slice(0, 80) : ''}</summary>` +
        cur.calls.map(([n, a, o]) => `<pre>$ ${esc(n)} ${esc(JSON.stringify(a))}\n${esc(String(o).slice(0, 1500))}</pre>`).join('') + '</details>';
      live().innerHTML = html;
    };
    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      buf += dec.decode(value, { stream: true });
      const parts = buf.split('\n\n');
      buf = parts.pop();
      for (const p of parts) {
        const line = p.split('\n').find(l => l.startsWith('data:'));
        if (!line) continue;
        let ev;
        try { ev = JSON.parse(line.slice(5).trim()); } catch { continue; }
        if (ev.kind === 'thought') { if (!cur) cur = { thought: '', calls: [] }; cur.thought = (cur.thought ? cur.thought + ' ' : '') + ev.text; }
        else if (ev.kind === 'token') { liveAnswer += ev.text || ''; }
        else if (ev.kind === 'tool_start') { if (!cur) cur = { thought: '', calls: [] }; cur.calls.push([ev.name, ev.args, '…']); }
        else if (ev.kind === 'tool_result') {
          if (cur) {
            const ix = cur.calls.findIndex(c => c[0] === ev.name && c[2] === '…');
            if (ix >= 0) cur.calls[ix][2] = ev.output + (ev.truncated ? '\n… (truncated)' : '');
            else cur.calls.push([ev.name, {}, ev.output]);
            steps.push(cur); cur = null;
          }
        }
        else if (ev.kind === 'final') { finalText = ev.text; if (cur) { steps.push(cur); cur = null; } }
        paintLive();
      }
    }
    askbody.innerHTML = `<h2>Answer</h2>${md(finalText || '(no answer)')}` +
      steps.map((s, i) => `<details><summary>Step ${i + 1}${s.thought ? ' — ' + esc(s.thought).slice(0, 80) : ''}</summary>` +
        s.calls.map(([n, a, o]) => `<pre>$ ${esc(n)} ${esc(JSON.stringify(a))}\n${esc(String(o).slice(0, 2000))}</pre>`).join('') + '</details>').join('');
  } catch (err) {
    // Fallback: classic JSON ask.
    try {
      const res = await apiAsk(question);
      renderTranscript(res.transcript);
    } catch (e2) {
      askbody.innerHTML = `<p>ask failed: ${esc(err.message || err)}</p><p>Set GEMINI_API_KEY where the server runs.</p>`;
    }
  }
}
function renderTranscript(t) {
  let html = `<h2>Answer</h2>${md(t.final_text || '(no answer)')}`;
  (t.steps || []).forEach((s, i) => {
    html += `<h2>Step ${i + 1}${s.thought ? ' — ' + esc(s.thought).slice(0, 80) : ''}</h2>`;
    (s.calls || []).forEach(([call, result]) => {
      html += `<pre>$ ${esc(call.name)} ${esc(JSON.stringify(call.args))}\n${esc(String(result.output).slice(0, 2000))}</pre>`;
    });
  });
  askbody.innerHTML = html;
}
askq.addEventListener('keydown', async (e) => {
  if (e.key !== 'Enter' || !askq.value.trim()) return;
  const v = askq.value.trim();
  askpanel.hidden = false;
  if (v.startsWith('>')) {
    const query = v.slice(1).trim();
    const hits = await apiSearch(query).catch(() => []);
    askbody.innerHTML = '<h2>Search</h2>' + (hits.map(h =>
      `<pre>${esc(h.path)}:${h.line} ${hi(String(h.text).slice(0, 160), query)}</pre>`).join('') || '<p>no matches</p>');
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
async function openPr(url) {
  askpanel.hidden = false;
  askbody.innerHTML = '<p>fetching PR…</p>';
  try {
    let s;
    if (invoke) s = await invoke('open_pr', { url });
    else throw new Error('PR mode needs the desktop app or `ferro <pr-url>` on the server');
    lastStats = s;
    allFiles = await apiFiles().catch(() => []);
    try { parseGitStatus(await apiGitStatus()); } catch {}
    renderSidebar(allFiles);
    status();
    const info = await apiPrInfo().catch(() => ({ pr: null }));
    prInfo = info && info.pr ? info.pr : null;
    askpanel.hidden = true;
    showHome();
  } catch (err) {
    askbody.innerHTML = `<p>open PR failed: ${esc(err.message || err)}</p>`;
  }
}
if (openBtn) openBtn.onclick = pickFolder;
$('ol-toggle').onclick = toggleOutline;
reindexBtn.onclick = async () => { await apiReindex(); await boot(true); };

document.addEventListener('keydown', (e) => {
  const mod = e.ctrlKey || e.metaKey;
  const k = e.key.toLowerCase();
  if (mod && e.shiftKey && k === 't') { e.preventDefault(); reopenTab(); }
  else if (mod && e.shiftKey && k === 'r') { e.preventDefault(); apiReindex().then(() => boot(true)); }
  else if (mod && e.key === 'Tab') { e.preventDefault(); cycleTab(e.shiftKey ? -1 : 1); }
  else if (e.altKey && /^[1-9]$/.test(e.key)) { e.preventDefault(); const t = tabs[+e.key - 1]; if (t) openFile(t); }
  else if (mod && k === 'b') { e.preventDefault(); document.body.classList.toggle('no-side'); }
  else if (mod && k === ',') { e.preventDefault(); openSettings(); }
  else if (mod && k === 'g') { e.preventDefault(); openPalette(':'); }
  else if (mod && k === 'f') { e.preventDefault(); openFind(); }
  else if (e.altKey && !mod && k === 'c') { e.preventDefault(); copyRef(); }
  else if (e.altKey && !mod && k === 'a') { e.preventDefault(); copyWithContext(); }
  else if (e.altKey && !mod && k === 'u') { e.preventDefault(); findUsages(); }
  else if (e.altKey && !mod && k === 'r') { e.preventDefault(); openCompose(); }
  else if (e.altKey && !mod && k === 'm') { e.preventDefault(); toggleMd(); }
  else if (cur.mode === 'image' && ['+', '=', '-', '_', '0', '1', 'b', 'p'].includes(e.key)) {
    e.preventDefault();
    if (e.key === '+' || e.key === '=') imgState.scale = Math.min(32, imgState.scale * 1.25);
    else if (e.key === '-' || e.key === '_') imgState.scale = Math.max(0.05, imgState.scale / 1.25);
    else if (e.key === '0') fitImage();
    else if (e.key === '1') { imgState.scale = 1; }
    else if (e.key === 'b' || e.key === 'B') imgState.bg = (imgState.bg + 1) % 3;
    else if (e.key === 'p' || e.key === 'P') imgEl.classList.toggle('pixel');
    if (e.key !== '0') applyImg();
  }
  else if (e.altKey && k === 'z') { e.preventDefault(); toggleWrap(); }
  else if (e.altKey && !mod && e.key === 'ArrowLeft') { e.preventDefault(); goHistory(-1); }
  else if (e.altKey && !mod && e.key === 'ArrowRight') { e.preventDefault(); goHistory(1); }
  else if (mod && k === 'k') { e.preventDefault(); pal.hidden ? openPalette() : closePalette(); }
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
let askSteps = 8;
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
  try {
    const s0 = await apiSettingsGet();
    if (typeof s0.askMaxSteps === 'number') askSteps = Math.min(16, Math.max(1, s0.askMaxSteps));
    applySettings(s0);
  } catch {}
  try {
    const info = await apiPrInfo();    prInfo = info && info.pr ? info.pr : null;
    const banner = $('prbanner');
    if (info && info.pr) {
      const pr = info.pr;
      banner.hidden = false;
      banner.innerHTML = `<span class="n">PR #${pr.number}</span>` +
        `<span>${esc(pr.owner)}/${esc(pr.repo)}</span>` +
        `<span class="mut">${esc(pr.base_ref)} ← head ${esc(String(pr.head_sha).slice(0, 8))}</span>` +
        `<span class="mut">merge-base diff</span>`;
      const btn = document.createElement('button');
      btn.textContent = 'View diff';
      btn.onclick = () => showDiff();
      banner.appendChild(btn);
    } else banner.hidden = true;
    await refreshDrafts().catch(() => []);
  } catch {}
  setTimeout(bootStats, 800);
  if (!invoke && openBtn) openBtn.style.display = 'none';
  if (reset && currentPath) openFile(currentPath);
  else {
    // Deep link: /?file=path&line=N (from `ferro file:line`).
    const params = new URLSearchParams(location.search);
    const f = params.get('file');
    if (f && allFiles.some(x => (typeof x === 'string' ? x : x.path) === f)) {
      const n = parseInt(params.get('line') || '1', 10) || 1;
      await openFile(f);
      viewport.scrollTop = Math.max(0, (n - 8) * ROW_H);
      paint();
      history.replaceState(null, '', location.pathname);
    } else showHome();
  }
}
boot(false);
