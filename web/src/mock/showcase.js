// A realistic change for commit views in mock mode (the live demo and README media):
// "feat(search): fuzzy scoring by path depth". Hunks are written as unified-diff lines and
// highlighted like the real backend (each side with its own syntax state).
import { highlight } from './hl.js';

const FILES = [
  {
    path: 'crates/ferro-core/src/fuzzy.rs',
    summary: 'Ranks shallow files above deep ones: score now takes ScoreOpts and subtracts a penalty per folder level.',
    notes: ['Takes the new ScoreOpts and subtracts a penalty for each folder level, so src/lib.rs outranks src/a/b/c/lib.rs.', 'Adds ScoreOpts, with a default penalty of 3 points per folder level.'],
    // A review of each hunk (verdict, why, and the suggested new side).
    reviews: [{
      verdict: 'improve',
      why: 'Clamping at 0 makes every deep match tie at 0, so their order is lost; keep the score signed.',
      code: [
        '/// Score `path` against `query`: higher is better, `None` when it does not match.',
        'pub fn score(query: &str, path: &str, opts: &ScoreOpts) -> Option<i64> {',
        '    let name = basename(path);',
        '    let mut s = subsequence_score(query, path)?;',
        '    if name.starts_with(query) {',
        '        s += 40;',
        '    }',
        '    // Shallow files win ties: `src/lib.rs` before `src/a/b/c/lib.rs`.',
        "    let depth = path.matches('/').count() as i64;",
        '    // Signed on purpose: clamping at 0 would make every deep match tie.',
        '    Some(s - depth * opts.depth_penalty)',
        '}',
      ].join('\n'),
    }],
    hunks: [
      {
        oldStart: 38, newStart: 38, section: 'pub fn score(query: &str, path: &str) -> Option<i64>',
        lines: [
          ' /// Score `path` against `query`: higher is better, `None` when it does not match.',
          '-pub fn score(query: &str, path: &str) -> Option<i64> {',
          '+pub fn score(query: &str, path: &str, opts: &ScoreOpts) -> Option<i64> {',
          '     let name = basename(path);',
          '     let mut s = subsequence_score(query, path)?;',
          '     if name.starts_with(query) {',
          '         s += 40;',
          '     }',
          '-    Some(s)',
          '+    // Shallow files win ties: `src/lib.rs` before `src/a/b/c/lib.rs`.',
          '+    let depth = path.matches(\'/\').count() as i64;',
          '+    s -= depth * opts.depth_penalty;',
          '+    Some(s.max(0))',
          ' }',
        ],
      },
      {
        oldStart: 71, newStart: 76, section: 'pub fn rank(query: &str, paths: &[String], limit: usize) -> Vec<Hit>',
        lines: [
          '     hits.truncate(limit);',
          '     hits',
          ' }',
          '+',
          '+/// How ranking weighs a match.',
          '+#[derive(Debug, Clone, Copy)]',
          '+pub struct ScoreOpts {',
          '+    /// Points taken off per directory level.',
          '+    pub depth_penalty: i64,',
          '+}',
          '+',
          '+impl Default for ScoreOpts {',
          '+    fn default() -> Self {',
          '+        Self { depth_penalty: 3 }',
          '+    }',
          '+}',
        ],
      },
    ],
  },
  {
    path: 'crates/ferro-core/src/search.rs',
    summary: 'Quick open passes the default ScoreOpts to the new score signature; results are otherwise unchanged.',
    notes: ['Builds the default ScoreOpts once and passes it to every score call.'],
    hunks: [
      {
        oldStart: 84, newStart: 84, section: 'pub fn quick_open(index: &FileIndex, q: &str, limit: usize) -> Vec<Hit>',
        lines: [
          ' pub fn quick_open(index: &FileIndex, q: &str, limit: usize) -> Vec<Hit> {',
          '+    let opts = ScoreOpts::default();',
          '     let mut hits: Vec<Hit> = index',
          '         .paths()',
          '-        .filter_map(|p| fuzzy::score(q, p).map(|s| Hit::new(p, s)))',
          '+        .filter_map(|p| fuzzy::score(q, p, &opts).map(|s| Hit::new(p, s)))',
          '         .collect();',
          '     hits.sort_by(|a, b| b.score.cmp(&a.score));',
        ],
      },
    ],
  },
  {
    path: 'crates/ferro-core/tests/fuzzy_golden.rs',
    summary: 'Adds a golden test for the new ranking rule.',
    notes: ['Checks that a shallow lib.rs scores higher than the same name four folders deep.'],
    hunks: [
      {
        oldStart: 52, newStart: 52, section: '',
        lines: [
          '     assert_eq!(top[0].path, "README.md");',
          ' }',
          '+',
          '+#[test]',
          '+fn shallow_paths_rank_first() {',
          '+    let opts = ScoreOpts::default();',
          '+    let top = score("lib", "src/lib.rs", &opts).unwrap();',
          '+    let deep = score("lib", "src/a/b/c/lib.rs", &opts).unwrap();',
          '+    assert!(top > deep, "{top} vs {deep}");',
          '+}',
        ],
      },
    ],
  },
];

const escape = (s) => s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');

/** Changed range between a deleted and an added line (UTF-16 offsets), for word highlights. */
function changed(a, b) {
  let p = 0;
  while (p < a.length && p < b.length && a[p] === b[p]) p++;
  let s = 0;
  while (s < a.length - p && s < b.length - p && a[a.length - 1 - s] === b[b.length - 1 - s]) s++;
  return [[p, a.length - s], [p, b.length - s]];
}

function build(file) {
  let additions = 0;
  let deletions = 0;
  const hunks = file.hunks.map((h, k) => {
    const oldSide = h.lines.filter((l) => l[0] !== '+').map((l) => l.slice(1));
    const newSide = h.lines.filter((l) => l[0] !== '-').map((l) => l.slice(1));
    let oldHtml;
    let newHtml;
    try {
      oldHtml = highlight(oldSide.join('\n'), 'rust');
      newHtml = highlight(newSide.join('\n'), 'rust');
    } catch {
      oldHtml = oldSide.map(escape);
      newHtml = newSide.map(escape);
    }
    let o = h.oldStart;
    let n = h.newStart;
    let oi = 0;
    let ni = 0;
    const rows = h.lines.map((l) => {
      const text = l.slice(1);
      if (l[0] === '-') { deletions++; return { t: 'del', o: o++, n: null, text, html: oldHtml[oi++] ?? escape(text) }; }
      if (l[0] === '+') { additions++; return { t: 'add', o: null, n: n++, text, html: newHtml[ni++] ?? escape(text) }; }
      oi++; ni++;
      return { t: 'ctx', o: o++, n: n++, text, html: newHtml[ni - 1] ?? escape(text) };
    });
    // Pair the k-th deleted row of a block with its k-th added row (like the backend).
    for (let i = 0; i < rows.length;) {
      const dels = [];
      const adds = [];
      while (rows[i]?.t === 'del') dels.push(rows[i++]);
      while (rows[i]?.t === 'add') adds.push(rows[i++]);
      if (!dels.length && !adds.length) { i++; continue; }
      for (let k = 0; k < Math.min(dels.length, adds.length); k++) {
        const [a, b] = changed(dels[k].text, adds[k].text);
        dels[k].ch = [a];
        adds[k].ch = [b];
      }
    }
    const oldLines = rows.filter((r) => r.t !== 'add').length;
    const newLines = rows.filter((r) => r.t !== 'del').length;
    return {
      id: `sc_${file.path.length}_${k}`,
      header: `@@ -${h.oldStart},${oldLines} +${h.newStart},${newLines} @@${h.section ? ` ${h.section}` : ''}`,
      section: h.section, oldStart: h.oldStart, oldLines, newStart: h.newStart, newLines, rows,
    };
  });
  return { path: file.path, status: 'M', binary: false, tooLarge: false, language: 'rust', hunks, additions, deletions, summary: file.summary, notes: file.notes, reviews: file.reviews };
}

let built = null;
const all = () => (built ||= FILES.map(build));

export function showcaseChanges() {
  return all().map((f) => ({ path: f.path, status: 'M', additions: f.additions, deletions: f.deletions, binary: false }));
}

export function showcaseDiff(path) {
  const f = all().find((x) => x.path === path);
  if (!f) return null;
  const { additions, deletions, summary, notes, ...diff } = f;
  return structuredClone(diff);
}

/** AI change notes for a showcase file (API.md § 10.8 shape), or null. */
export function showcaseExplain(path) {
  const f = all().find((x) => x.path === path);
  if (!f) return null;
  return {
    path,
    summary: f.summary,
    hunks: f.hunks.map((h, i) => {
      const r = f.reviews?.[i];
      const kind = h.rows.some((x) => x.t === 'del') ? 'changed' : 'added';
      const out = { id: h.id, kind, note: f.notes[i] || f.notes[0], verdict: r?.verdict || 'ok' };
      if (r?.why) out.why = r.why;
      if (r?.code) {
        const side = h.rows.filter((x) => x.t !== 'del');
        out.suggestion = { start: side[0].n, end: side[side.length - 1].n, original: side.map((x) => x.text).join('\n'), code: r.code };
      }
      return out;
    }),
    cached: false,
  };
}
