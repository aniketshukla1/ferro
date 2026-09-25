// Mock-only fuzzy scorer that honors the ranking contract in docs/spec/API.md § 5.1:
// exact basename first, tight clusters, shallow paths win ties. Returns UTF-16 positions.

const isBoundary = (c) => c === '/' || c === '_' || c === '-' || c === '.' || c === ' ' || c === '@';

function matchFrom(q, lower, start) {
  // forward: earliest completion at or after start
  let qi = 0;
  let end = -1;
  for (let i = start; i < lower.length && qi < q.length; i++) {
    if (lower[i] === q[qi]) {
      qi++;
      if (qi === q.length) end = i;
    }
  }
  if (end < 0) return null;
  // backward from end: tightest cluster ending at `end`
  const pos = new Array(q.length);
  let k = q.length - 1;
  for (let i = end; i >= start && k >= 0; i--) {
    if (lower[i] === q[k]) pos[k--] = i;
  }
  return k < 0 ? pos : null;
}

function scorePositions(path, lower, pos, baseStart, q) {
  let s = 0;
  for (let k = 0; k < pos.length; k++) {
    const i = pos[k];
    const prev = i > 0 ? path[i - 1] : '/';
    if (i === 0 || isBoundary(prev)) s += 16;
    else if (prev >= 'a' && prev <= 'z' && path[i] >= 'A' && path[i] <= 'Z') s += 14;
    if (i >= baseStart) s += 14;
    if (k > 0) {
      const gap = i - pos[k - 1] - 1;
      if (gap === 0) s += 12;
      else s -= Math.min(gap, 12);
    }
  }
  if (pos[0] === baseStart) s += 20;
  const base = lower.slice(baseStart);
  if (base.includes(q)) s += 40;
  const dot = base.lastIndexOf('.');
  if ((dot > 0 ? base.slice(0, dot) : base) === q || base === q) s += 60;
  s -= Math.floor(path.length / 8);
  let depth = 0;
  for (let i = 0; i < path.length; i++) if (path[i] === '/') depth++;
  s -= depth * 2;
  return s;
}

/** @returns {{score:number, positions:number[]} | null} */
export function fuzzyScore(query, path, lower = path.toLowerCase()) {
  const q = query.toLowerCase().replace(/\s+/g, '');
  if (!q) return { score: 0, positions: [] };
  const baseStart = path.lastIndexOf('/') + 1;
  let best = null;
  // Prefer a match inside the basename when one exists.
  const inBase = matchFrom(q, lower, baseStart);
  if (inBase) best = { positions: inBase, score: scorePositions(path, lower, inBase, baseStart, q) };
  const any = matchFrom(q, lower, 0);
  if (any) {
    const s = scorePositions(path, lower, any, baseStart, q);
    if (!best || s > best.score) best = { positions: any, score: s };
  }
  return best;
}

export function fuzzyRank(query, entries, limit, boost = []) {
  const boostSet = new Set(boost);
  const out = [];
  for (const e of entries) {
    const r = fuzzyScore(query, e.path, e.lower);
    if (!r) continue;
    if (boostSet.has(e.path)) r.score += 25;
    out.push({ path: e.path, score: r.score, positions: r.positions });
  }
  out.sort((a, b) => b.score - a.score || a.path.length - b.path.length || (a.path < b.path ? -1 : 1));
  return { total: out.length, results: out.slice(0, limit) };
}
