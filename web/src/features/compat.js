// TEMPORARY compatibility shim: while backend B2a (fuzzy.v2 / search.v2) is pending, file and
// content search fall back to the legacy /api/fuzzy and /api/search endpoints and adapt their
// responses to the v1 shapes. Delete this file at the F1 flip once B2a has shipped.
import { ApiError } from '../core/api.js';
import { match } from '../core/match.js';

async function legacy(path, query, signal) {
  const url = new URL(`api/${path}`, document.baseURI);
  for (const [k, v] of Object.entries(query)) url.searchParams.set(k, String(v));
  const res = await fetch(url, { signal, credentials: 'same-origin' });
  if (!res.ok) throw new ApiError(res.status, res.status === 401 ? 'unauthorized' : 'unsupported', 'search is not available on this server yet');
  return res.json();
}

export async function compatFuzzy(q, limit, signal) {
  const t0 = performance.now();
  const rows = await legacy('fuzzy', { q, limit }, signal);
  return {
    q,
    total: rows.length,
    ms: performance.now() - t0,
    results: rows.map((r) => ({ path: r.path, score: r.score, positions: match(q, r.path)?.positions || [] })),
  };
}

export async function compatSearch(params, signal) {
  const t0 = performance.now();
  const rows = await legacy('search', { q: params.q, limit: 200 }, signal);
  const needle = params.q.toLowerCase();
  const byPath = new Map();
  for (const r of rows) {
    if (!byPath.has(r.path)) {
      if (byPath.size >= (params.maxFiles || 200)) continue;
      byPath.set(r.path, []);
    }
    const hits = byPath.get(r.path);
    if (hits.length >= (params.maxPerFile || 20)) continue;
    const ranges = [];
    const low = r.text.toLowerCase();
    for (let i = low.indexOf(needle); i >= 0 && ranges.length < 20; i = low.indexOf(needle, i + needle.length)) {
      ranges.push([i, i + needle.length]);
    }
    hits.push({ line: r.line, text: r.text, ranges, cutStart: false, cutEnd: false });
  }
  const files = [...byPath].map(([path, hits]) => ({ path, hits, more: false }));
  return { q: params.q, engine: 'scan', ms: performance.now() - t0, filesScanned: 0, filesMatched: files.length, truncated: false, excluded: { globs: [], files: 0 }, files };
}

// Until backend B3 ships `git.status.v2`, parse the legacy `git status --porcelain -b` text
// from /api/git-status into the v1 GitStatus shape (API.md § 6.1).
const CONFLICT = new Set(['DD', 'AU', 'UD', 'UA', 'DU', 'AA', 'UU']);

/** Undo git's C-style path quoting ("a\tb", octal UTF-8 bytes). */
function unquote(p) {
  if (!p.startsWith('"')) return p;
  const bytes = [];
  const s = p.slice(1, -1);
  for (let i = 0; i < s.length; i++) {
    const c = s[i];
    if (c !== '\\') { bytes.push(...new TextEncoder().encode(c)); continue; }
    const n = s[++i];
    if (/[0-7]/.test(n)) { bytes.push(parseInt(s.slice(i, i + 3), 8)); i += 2; }
    else bytes.push({ n: 10, t: 9, r: 13, '"': 34, '\\': 92, a: 7, b: 8, f: 12, v: 11 }[n] ?? n.charCodeAt(0));
  }
  return new TextDecoder().decode(new Uint8Array(bytes));
}

export function parsePorcelain(text) {
  const out = { branch: null, detached: false, headSha: null, upstream: null, ahead: 0, behind: 0, files: [], counts: { staged: 0, unstaged: 0, untracked: 0, conflicted: 0 } };
  for (const line of text.split('\n')) {
    if (!line) continue;
    if (line.startsWith('## ')) {
      let head = line.slice(3).replace(/^(No commits yet on |Initial commit on )/, '');
      if (head.startsWith('HEAD (no branch)')) { out.detached = true; continue; }
      let bracket = '';
      const b = head.indexOf(' [');
      if (b >= 0) { bracket = head.slice(b + 2, -1); head = head.slice(0, b); }
      const [branch, upstream] = head.split('...');
      out.branch = branch;
      out.upstream = upstream || null;
      out.ahead = Number(/ahead (\d+)/.exec(bracket)?.[1] || 0);
      out.behind = Number(/behind (\d+)/.exec(bracket)?.[1] || 0);
      continue;
    }
    const xy = line.slice(0, 2);
    let rest = line.slice(3);
    let origPath;
    if ((xy[0] === 'R' || xy[0] === 'C') && rest.includes(' -> ')) {
      const i = rest.indexOf(' -> ');
      origPath = unquote(rest.slice(0, i));
      rest = rest.slice(i + 4);
    }
    let path = unquote(rest);
    const dir = path.endsWith('/');
    if (dir) path = path.slice(0, -1);
    const untracked = xy === '??';
    const conflicted = CONFLICT.has(xy);
    const code = (c) => (c === ' ' || c === '?' || c === '!' ? null : c);
    const f = { path, index: untracked ? null : code(xy[0]), worktree: untracked ? null : code(xy[1]), untracked, conflicted };
    if (origPath) f.origPath = origPath;
    if (dir) f.dir = true;
    out.files.push(f);
    if (untracked) out.counts.untracked++;
    else if (conflicted) out.counts.conflicted++;
    else {
      if (f.index) out.counts.staged++;
      if (f.worktree) out.counts.unstaged++;
    }
  }
  return out;
}

export async function compatGitStatus(signal) {
  const url = new URL('api/git-status', document.baseURI);
  const res = await fetch(url, { signal, credentials: 'same-origin' });
  if (!res.ok) throw new ApiError(res.status, 'unsupported', 'git status is not available');
  return parsePorcelain(await res.text());
}
