const q = document.getElementById('q');
const filesEl = document.getElementById('files');
const codeEl = document.getElementById('code');
const statsEl = document.getElementById('stats');
let allFiles = [];

async function boot() {
  const s = await (await fetch('/api/stats')).json();
  statsEl.textContent = `${s.files} files · ${s.indexed_ms}ms · ${s.root}`;
  allFiles = await (await fetch('/api/files')).json();
  render(allFiles.slice(0, 200));
  // poll stats once after index warms up
  setTimeout(bootStats, 800);
}
async function bootStats() {
  try {
    const s = await (await fetch('/api/stats')).json();
    statsEl.textContent = `${s.files} files · ${s.indexed_ms}ms · ${s.root}`;
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
  const r = await fetch('/api/file?path=' + encodeURIComponent(path));
  const t = await r.text();
  codeEl.textContent = `// ${path}\n\n` + t.slice(0, 200000);
}
let deb = null;
q.addEventListener('input', () => {
  clearTimeout(deb);
  deb = setTimeout(async () => {
    const v = q.value.trim();
    if (!v) { render(allFiles.slice(0, 200)); return; }
    if (v.startsWith('>') || v.startsWith('/')) {
      const hits = await (await fetch('/api/search?q=' + encodeURIComponent(v.replace(/^>\s?\/\s?/, '')) + '&limit=50')).json();
      filesEl.innerHTML = '';
      for (const h of hits) {
        const d = document.createElement('div');
        d.className = 'f';
        d.textContent = `${h.path}:${h.line} ${h.text.slice(0, 120)}`;
        d.onclick = () => openFile(h.path);
        filesEl.appendChild(d);
      }
      return;
    }
    const res = await (await fetch('/api/fuzzy?q=' + encodeURIComponent(v) + '&limit=50')).json();
    render(res.map(r => r.path));
  }, 60);
});
document.addEventListener('keydown', (e) => {
  if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === 'p') { e.preventDefault(); q.focus(); q.select(); }
  if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === 'd') {
    e.preventDefault();
    fetch('/api/diff').then(r => r.text()).then(t => { codeEl.textContent = t.slice(0, 200000) || '(clean)'; });
  }
});
boot();
