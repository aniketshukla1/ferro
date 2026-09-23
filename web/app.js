const q = document.getElementById('q');
const filesEl = document.getElementById('files');
const statsEl = document.getElementById('stats');
const openBtn = document.getElementById('open');
const viewport = document.getElementById('viewport');
const spacer = document.getElementById('spacer');
const rowsEl = document.getElementById('rows');
const plainEl = document.getElementById('plain');
let allFiles = [];

// Dual backend: Tauri (window.__TAURI__.core.invoke) or HTTP (ferro serve).
const tauriInvoke = window?.__TAURI__?.core?.invoke ?? null;
const backend = tauriInvoke ? 'tauri' : 'http';
const invoke = tauriInvoke;

const ROW_H = 20;
const OVERSCAN = 24;
const WIN = 200;

const cur = { path: null, total: 0, cache: new Map(), mode: 'plain', pending: new Set() };

async function apiStats() {
  if (invoke) return await invoke('get_stats');
  return await (await fetch('/api/stats')).json();
}
async function apiFiles() {
  if (invoke) return await invoke('list_files');
  return await (await fetch('/api/files')).json();
}
async function apiFuzzy(v) {
  if (invoke) return await invoke('fuzzy', { q: v, limit: 50 });
  return await (await fetch('/api/fuzzy?q=' + encodeURIComponent(v) + '&limit=50')).json();
}
async function apiSearch(v) {
  if (invoke) return await invoke('grep', { q: v, limit: 50 });
  return await (await fetch('/api/search?q=' + encodeURIComponent(v) + '&limit=50')).json();
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

function esc(s) {
  return String(s).replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
}

function showFileMode() {
  cur.mode = 'file';
  plainEl.hidden = true;
  viewport.hidden = false;
}
function showPlainMode() {
  cur.mode = 'plain';
  viewport.hidden = true;
  plainEl.hidden = false;
}

async function openFile(path) {
  showFileMode();
  cur.path = path;
  cur.cache.clear();
  cur.pending.clear();
  let meta;
  try {
    meta = await apiMeta(path);
  } catch {
    showPlainMode();
    plainEl.textContent = 'cannot open ' + path;
    return;
  }
  cur.total = meta.total_lines || 0;
  spacer.style.height = Math.max(1, cur.total) * ROW_H + 'px';
  viewport.scrollTop = 0;
  await ensureAround(0);
  paint();
}

async function ensureAround(first) {
  if (!cur.path) return;
  const start = Math.max(0, first - OVERSCAN);
  // Align fetches to 100-line blocks to maximize cache hits.
  const aligned = Math.floor(start / 100) * 100;
  const key = aligned;
  if (cur.cache.has(key) || cur.pending.has(key)) return;
  cur.pending.add(key);
  try {
    let win;
    try {
      win = await apiHighlight(cur.path, aligned, WIN);
      // Normalize to {total, start, lines:[{n, html}]}; fallback shape handled below.
      if (!win.lines && win.total === undefined) throw new Error('bad hl');
    } catch {
      const raw = await apiWindow(cur.path, aligned, WIN);
      win = { total: raw.total, start: raw.start, lines: raw.lines.map(l => ({ n: l.n, html: esc(l.text) })) };
    }
    cur.total = win.total || cur.total;
    spacer.style.height = Math.max(1, cur.total) * ROW_H + 'px';
    cur.cache.set(key, win);
    // Keep cache bounded: last 8 windows (~1600 lines).
    if (cur.cache.size > 8) {
      const oldest = cur.cache.keys().next().value;
      cur.cache.delete(oldest);
    }
  } finally {
    cur.pending.delete(key);
  }
}

function lineAt(n) {
  for (const win of cur.cache.values()) {
    for (const l of win.lines) {
      if (l.n === n) return l;
    }
  }
  return null;
}

let paintQueued = false;
function paint() {
  if (cur.mode !== 'file') return;
  const first = Math.max(1, Math.floor(viewport.scrollTop / ROW_H) + 1);
  const visible = Math.ceil(viewport.clientHeight / ROW_H) + 1;
  const from = Math.max(1, first - OVERSCAN);
  const to = Math.min(cur.total, first + visible + OVERSCAN);
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
  ensureAround(first - 1);
}

let scrollRaf = false;
viewport.addEventListener('scroll', () => {
  if (scrollRaf) return;
  scrollRaf = true;
  requestAnimationFrame(() => {
    scrollRaf = false;
    paint();
    // Re-paint after pending fetch resolves.
    setTimeout(() => { if (!paintQueued) { paintQueued = true; setTimeout(() => { paintQueued = false; paint(); }, 120); } }, 50);
  });
});

async function boot() {
  const s = await apiStats().catch(() => ({ files: 0, indexed_ms: 0, root: '' }));
  statsEl.textContent = `[${backend}] ${s.files} files · ${s.indexed_ms}ms · ${s.root}`;
  allFiles = await apiFiles().catch(() => []);
  render(allFiles.slice(0, 200));
  setTimeout(bootStats, 800);
  if (!invoke && openBtn) openBtn.style.display = 'none';
}
async function bootStats() {
  try {
    const s = await apiStats();
    statsEl.textContent = `[${backend}] ${s.files} files · ${s.indexed_ms}ms · ${s.root}`;
  } catch {}
}
function render(list) {
  filesEl.innerHTML = '';
  for (const f of list) {
    const d = document.createElement('div');
    d.className = 'f';
    d.textContent = (typeof f === 'string') ? f : f.path;
    d.onclick = () => openFile((typeof f === 'string') ? f : f.path);
    filesEl.appendChild(d);
  }
}
let deb = null;
q.addEventListener('input', () => {
  clearTimeout(deb);
  deb = setTimeout(async () => {
    const v = q.value.trim();
    if (!v) { render(allFiles.slice(0, 200)); return; }
    if (v.startsWith('>') || v.startsWith('/')) {
      const query = v.replace(/^>\s?\/\s?/, '').replace(/^>/, '').replace(/^\//, '');
      const hits = await apiSearch(query);
      filesEl.innerHTML = '';
      for (const h of hits) {
        const d = document.createElement('div');
        d.className = 'f';
        d.textContent = `${h.path}:${h.line} ${String(h.text).slice(0, 120)}`;
        d.onclick = () => openFile(h.path);
        filesEl.appendChild(d);
      }
      return;
    }
    const res = await apiFuzzy(v);
    render(res.map(r => r.path));
  }, 60);
});
document.addEventListener('keydown', (e) => {
  if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === 'p') { e.preventDefault(); q.focus(); q.select(); }
  if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === 'd') {
    e.preventDefault();
    apiDiff().then(t => { showPlainMode(); plainEl.textContent = String(t).slice(0, 200000) || '(clean)'; });
  }
  if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === 'o') {
    e.preventDefault();
    pickFolder();
  }
});
async function pickFolder() {
  if (!invoke) return;
  const dir = await invoke('pick_folder');
  if (dir) {
    const s = await invoke('set_root', { path: dir });
    statsEl.textContent = `[${backend}] ${s.files} files · ${s.indexed_ms}ms · ${s.root}`;
    allFiles = await apiFiles();
    render(allFiles.slice(0, 200));
  }
}
if (openBtn) openBtn.onclick = pickFolder;
boot();
