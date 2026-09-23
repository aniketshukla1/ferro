const q = document.getElementById('q');
const filesEl = document.getElementById('files');
const codeEl = document.getElementById('code');
const statsEl = document.getElementById('stats');
const openBtn = document.getElementById('open');
let allFiles = [];

// Dual backend: Tauri (window.__TAURI__.core.invoke) or HTTP (ferro serve).
const tauriInvoke = window?.__TAURI__?.core?.invoke ?? null;
const backend = tauriInvoke ? 'tauri' : 'http';
const invoke = tauriInvoke;

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
async function apiFile(path) {
  if (invoke) return await invoke('read_file', { path });
  const r = await fetch('/api/file?path=' + encodeURIComponent(path));
  return await r.text();
}
async function apiDiff() {
  if (invoke) return await invoke('git_diff', { path: null });
  return await (await fetch('/api/diff')).text();
}

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
async function openFile(path) {
  const t = await apiFile(path);
  codeEl.textContent = `// ${path}\n\n` + String(t).slice(0, 200000);
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
    apiDiff().then(t => { codeEl.textContent = String(t).slice(0, 200000) || '(clean)'; });
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
