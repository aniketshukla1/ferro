// Client-side fuzzy matching for short lists (commands, themes, symbols, settings).
// Files use the server's /fuzzy; this is for things the client already has.

const boundary = (c) => c === ' ' || c === '/' || c === '_' || c === '-' || c === '.' || c === ':' || c === '(';

/** @returns {{score:number, positions:number[]} | null} */
export function match(query, text) {
  const q = query.toLowerCase().trim();
  if (!q) return { score: 0, positions: [] };
  const t = text.toLowerCase();
  // contiguous substring beats scattered matches
  const at = t.indexOf(q);
  if (at >= 0) {
    const positions = Array.from({ length: q.length }, (_, i) => at + i);
    const score = 100 - at + (at === 0 || boundary(text[at - 1]) ? 30 : 0) - text.length / 10;
    return { score, positions };
  }
  const positions = [];
  let qi = 0;
  let score = 0;
  let prev = -2;
  for (let i = 0; i < t.length && qi < q.length; i++) {
    if (q[qi] === ' ') { qi++; i--; continue; }
    if (t[i] !== q[qi]) continue;
    positions.push(i);
    if (i === prev + 1) score += 8;
    if (i === 0 || boundary(text[i - 1])) score += 10;
    else if (text[i] >= 'A' && text[i] <= 'Z' && text[i - 1] >= 'a' && text[i - 1] <= 'z') score += 8;
    score -= Math.min(i - prev - 1, 6) * (prev < 0 ? 0.2 : 1);
    prev = i;
    qi++;
  }
  while (qi < q.length && q[qi] === ' ') qi++;
  if (qi < q.length) return null;
  return { score: score - text.length / 10, positions };
}

/** Filter + rank items by a key. Returns [{item, score, positions}]. */
export function rank(query, items, key = (x) => x, limit = 100) {
  const out = [];
  for (const item of items) {
    const r = match(query, key(item));
    if (r) out.push({ item, ...r });
  }
  if (query.trim()) out.sort((a, b) => b.score - a.score);
  return out.slice(0, limit);
}
