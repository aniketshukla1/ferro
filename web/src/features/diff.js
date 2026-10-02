// Diff view (FRONTEND.md § 6.10–6.12, API.md § 6.3–6.4): one continuous multi-file diff on the core
// VirtualList with measured item heights. Split and unified layouts, whitespace toggle, file headers,
// hunk expanders, intraline highlights, image diffs; plus code-view gutter markers with mini-diffs.
import { h, mount, setTrustedHTML, textRange } from '../core/dom.js';
import { request, has } from '../core/api.js';
import { store } from '../core/store.js';
import { bus } from '../core/bus.js';
import { debounce, plural } from '../core/util.js';
import { VirtualList } from '../core/virtual.js';
import { heightModel } from '../core/heights.js';
import { icon } from '../ui/icons.js';
import { toast } from '../ui/overlay.js';
import {
  createInlineThread, createInlineComposer, createInlineDraft, toggleFileViewed,
  getThreads, getDrafts, getViewedState, getActiveComposer, setActiveComposer,
} from './review.js';

// Raster formats git treats as binary. SVG is text: it gets the image stage above its source hunks.
const IMAGE_EXT = /\.(png|jpe?g|gif|webp|ico|icns|bmp)$/i;
const SVG = /\.svg$/i;
const HEIGHTS = { 'file-header': 38, 'hunk-sep': 26, 'hunk-note': 26, 'file-note': 28, 'too-large': 60, binary: 60, error: 60, image: 360 };
const itemHeight = (item) => (item?.type === 'annotation' ? item.estH : HEIGHTS[item?.type] || 20);

/** First-paint height guess for a stack of thread/draft/composer cards; the view then measures it. */
function estimateAnnotationHeight(threadsAt, draftsAt, composerHere) {
  let h = 16;
  for (const t of threadsAt) h += t.outdated ? 40 : 110 + (t.comments?.length || 1) * 52;
  for (const d of draftsAt) h += 72 + Math.ceil((d.body || '').length / 80) * 18;
  if (composerHere) h += 190;
  return h;
}

/**
 * What a file's diff renders as. Every modified file carries blob ids (`index a..b`), so the
 * image stage keys on git's own binary verdict plus a raster extension, never on blob ids.
 * @returns {'image'|'binary'|'too-large'|'svg'|'hunks'} ('svg': image stage plus the source hunks)
 */
export function diffKind(path, diff) {
  if (diff.binary) return IMAGE_EXT.test(path) ? 'image' : 'binary';
  if (diff.tooLarge) return 'too-large';
  return SVG.test(path) ? 'svg' : 'hunks';
}

/** Before/after URLs of an image diff; null for the side that does not exist (added/deleted). */
export function imageSides(diff, { baseRev = 'HEAD', target = 'worktree' } = {}) {
  const raw = (rev, path) => `api/v1/git/blob/raw?rev=${encodeURIComponent(rev)}&path=${encodeURIComponent(path)}`;
  const added = diff.status === 'A' || diff.status === '?' || !diff.oldBlob;
  const deleted = diff.status === 'D' || !diff.newBlob;
  return {
    before: added ? null : raw(baseRev, diff.oldPath || diff.path),
    after: deleted ? null : raw(target, diff.path),
  };
}

/**
 * New-side lines a hunk can grow into: `[from, to]` inclusive, or null when it already touches
 * its neighbour (or the file start). `to` is Infinity below the last hunk (the server stops at EOF).
 */
export function hunkGap(hunks, k, dir) {
  const hk = hunks[k];
  if (dir === 'up') {
    const prev = hunks[k - 1];
    const from = prev ? prev.newStart + prev.newLines : 1;
    const to = hk.newStart - 1;
    return from <= to ? [from, to] : null;
  }
  const next = hunks[k + 1];
  const from = hk.newStart + hk.newLines;
  const to = next ? next.newStart - 1 : Infinity;
  return from <= to ? [from, to] : null;
}

/**
 * Grow a hunk by context lines read from the new side (`lines`: `{n, html?, text?}` ascending).
 * Context is unchanged text, so old-side numbers follow from the hunk's own offset on that side.
 */
export function expandHunk(hunk, dir, lines) {
  if (!lines.length) return hunk;
  const delta = dir === 'up'
    ? hunk.newStart - hunk.oldStart
    : (hunk.newStart + hunk.newLines) - (hunk.oldStart + hunk.oldLines);
  const rows = lines.map((l) => ({ t: 'ctx', o: l.n - delta, n: l.n, html: l.html, text: l.text }));
  if (dir === 'up') {
    hunk.rows = [...rows, ...(hunk.rows || [])];
    hunk.newStart = rows[0].n;
    hunk.oldStart = rows[0].o;
  } else {
    hunk.rows = [...(hunk.rows || []), ...rows];
  }
  hunk.newLines += rows.length;
  hunk.oldLines += rows.length;
  hunk.header = `@@ -${hunk.oldStart},${hunk.oldLines} +${hunk.newStart},${hunk.newLines} @@${hunk.section ? ` ${hunk.section}` : ''}`;
  return hunk;
}

/** Row code: the server's escaped highlight HTML when present, else plain text (never parsed). */
function setCode(el, row) {
  if (row && row.html != null) setTrustedHTML(el, row.html, 'hl');
  else el.textContent = row?.text ?? '';
}

// ---------- intraline highlights (CSS Custom Highlight API) ----------
const canHighlight = () => typeof CSS !== 'undefined' && !!CSS.highlights && typeof Highlight === 'function';

/** Drop the intraline ranges a code cell registered (items are pooled and repainted). */
function clearIntraline(codeEl) {
  const prev = codeEl?.__hl;
  if (!prev) return;
  for (const [name, rng] of prev) CSS.highlights.get(name)?.delete(rng);
  codeEl.__hl = null;
}

/** Register a cell's intraline ranges, replacing the ones it held before (no growth on repaint). */
function applyIntraline(codeEl, chRanges, isAdd) {
  clearIntraline(codeEl);
  if (!chRanges?.length || !canHighlight()) return;
  const name = isAdd ? 'ferro-diff-add' : 'ferro-diff-del';
  let hl = CSS.highlights.get(name);
  if (!hl) CSS.highlights.set(name, (hl = new Highlight()));
  const held = [];
  for (const [a, b] of chRanges) {
    const rng = textRange(codeEl, a, b);
    if (rng) {
      hl.add(rng);
      held.push([name, rng]);
    }
  }
  codeEl.__hl = held.length ? held : null;
}

// ---------- items ----------
const expandBtn = (cls, label, aria) => h('button', { class: `btn xs ${cls}`, 'aria-label': aria }, label);
const banner = (kind, ic, action) => h('div', { class: `diff-banner ${kind}` }, icon(ic, 'sm'), h('span', { class: 'diff-banner-text' }), action);

const KIND_LABEL = { added: 'Added', removed: 'Removed', changed: 'Changed' };
const VERDICT_LABEL = { improve: 'Could be better', problem: 'Problem' };

function createItem(type, layout) {
  switch (type) {
    case 'file-header':
      return h('div', { class: 'diff-file-header' },
        h('button', { class: 'icon-btn xs diff-collapse-btn', 'aria-label': 'Toggle file collapse' }, icon('chevron-down', 'xs')),
        h('span', { class: 'gitc M diff-status-badge' }),
        h('span', { class: 'diff-file-path truncate' }),
        h('span', { class: 'diff-file-stats num' }, h('span', { class: 'diff-add-stat' }), h('span', { class: 'diff-del-stat' })),
        h('div', { class: 'diff-file-actions' },
          h('label', { class: 'diff-viewed-label' }, h('input', { type: 'checkbox', class: 'diff-viewed-chk', 'aria-label': 'Viewed' }), 'Viewed'),
          h('button', { class: 'icon-btn xs diff-open-btn', 'aria-label': 'Open in editor', 'data-tip': 'Open file' }, icon('file-text', 'xs')),
          h('button', { class: 'icon-btn xs diff-copy-btn', 'aria-label': 'Copy path', 'data-tip': 'Copy path' }, icon('copy', 'xs'))));
    case 'hunk-sep':
      return h('div', { class: 'diff-hunk-sep' },
        h('span', { class: 'diff-hunk-header' }),
        h('div', { class: 'diff-hunk-actions' },
          expandBtn('diff-expand-up-btn', '↑ 20', 'Expand up 20 lines'),
          expandBtn('diff-expand-down-btn', '↓ 20', 'Expand down 20 lines'),
          expandBtn('diff-expand-all-btn', 'All', 'Expand all'),
          h('button', { class: 'btn xs diff-hunk-action-btn', hidden: true })));
    case 'file-note':
    case 'hunk-note':
      // AI change notes (API.md § 10.8): one line; the full text is the tooltip.
      return h('div', { class: `diff-note ${type}` }, h('span', { class: 'diff-note-kind' }), h('button', { class: 'diff-note-verdict', type: 'button' }), h('span', { class: 'diff-note-text' }));
    case 'too-large':
      return banner('too-large', 'alert', h('button', { class: 'btn sm diff-load-large-btn' }, 'Load diff'));
    case 'binary':
      return banner('binary', 'file', h('a', { class: 'btn sm', target: '_blank', rel: 'noopener noreferrer' }, 'Open raw'));
    case 'error':
      return banner('error', 'alert', h('button', { class: 'btn sm diff-retry-btn' }, 'Retry'));
    case 'image':
      return h('div', { class: 'diff-image-view' },
        h('div', { class: 'diff-image-toolbar' },
          h('div', { class: 'btn-group' }, [['side', 'Side-by-side'], ['swipe', 'Swipe'], ['onion', 'Onion-skin']]
            .map(([mode, label]) => h('button', { class: 'btn xs diff-img-mode', 'data-mode': mode }, label)))),
        h('div', { class: 'diff-image-stage' }));
    case 'annotation':
      return h('div', { class: 'diff-annotation-slot' });
    default: { // diff-row
      const add = () => h('button', { class: 'diff-comment-add-btn', type: 'button', tabindex: '-1', 'aria-label': 'Add comment' }, icon('plus', 'xs'));
      if (layout === 'split') {
        const cell = (side) => h('div', { class: `diff-cell ${side}` }, h('span', { class: 'diff-ln num' }), h('span', { class: 'diff-sign' }), h('span', { class: 'diff-code' }), add(), h('button', { class: 'diff-finding-dot', type: 'button', hidden: true, 'aria-label': 'AI finding' }));
        return h('div', { class: 'diff-row split' }, cell('old'), cell('new'));
      }
      return h('div', { class: 'diff-row unified' },
        h('span', { class: 'diff-ln-old num' }), h('span', { class: 'diff-ln-new num' }), h('span', { class: 'diff-sign' }), h('span', { class: 'diff-code' }), add(), h('button', { class: 'diff-finding-dot', type: 'button', hidden: true, 'aria-label': 'AI finding' }));
    }
  }
}

function updateItem(el, item, layout, findingsCtx) {
  const q = (sel) => el.querySelector(sel);
  switch (item.type) {
    case 'file-header': {
      q('.diff-file-path').textContent = item.oldPath && item.oldPath !== item.path ? `${item.oldPath} → ${item.path}` : item.path;
      const badge = q('.diff-status-badge');
      badge.textContent = item.status || 'M';
      badge.className = `gitc ${item.status || 'M'} diff-status-badge`;
      q('.diff-add-stat').textContent = `+${item.additions || 0}`;
      q('.diff-del-stat').textContent = `-${item.deletions || 0}`;
      const chk = q('.diff-viewed-chk');
      chk.checked = !!item.viewed;
      chk.onchange = () => item.onToggleViewed?.(item.path, chk.checked);
      el.classList.toggle('collapsed', !!item.collapsed);
      const collapse = q('.diff-collapse-btn');
      collapse.classList.toggle('collapsed', !!item.collapsed);
      collapse.setAttribute('aria-expanded', String(!item.collapsed));
      collapse.onclick = (e) => { e.stopPropagation(); item.onToggleCollapse?.(item.path); };
      q('.diff-open-btn').onclick = (e) => { e.stopPropagation(); item.onOpen?.(item.path); };
      q('.diff-copy-btn').onclick = (e) => {
        e.stopPropagation();
        navigator.clipboard?.writeText(item.path)?.catch(() => {});
        toast({ kind: 'ok', title: 'Copied path', message: item.path, timeout: 1500 });
      };
      break;
    }
    case 'hunk-sep': {
      q('.diff-hunk-header').textContent = item.hunk?.header || '';
      const [up, down, all] = el.querySelectorAll('.diff-hunk-actions button');
      up.hidden = !item.canUp;
      down.hidden = !item.canDown;
      all.hidden = !item.canUp && !item.canDown;
      const act = q('.diff-hunk-action-btn');
      act.hidden = !item.action;
      act.textContent = item.action?.label || '';
      act.setAttribute('aria-label', item.action ? `${item.action.label} this hunk` : '');
      act.onclick = item.action ? () => item.action.run() : null;
      up.onclick = () => item.onExpand?.('up');
      down.onclick = () => item.onExpand?.('down');
      all.onclick = () => item.onExpand?.('all');
      break;
    }
    case 'file-note':
    case 'hunk-note': {
      const kind = q('.diff-note-kind');
      kind.textContent = item.type === 'file-note' ? '✦ AI' : KIND_LABEL[item.kind] || 'Changed';
      kind.className = `diff-note-kind ${item.kind || 'summary'}`;
      // The change's verdict (explain.js): a click opens why, and the suggested code.
      const v = q('.diff-note-verdict');
      v.textContent = VERDICT_LABEL[item.verdict] || '';
      v.className = `diff-note-verdict ${item.verdict || ''}`;
      v.hidden = !VERDICT_LABEL[item.verdict];
      q('.diff-note-text').textContent = item.why ? `${item.note} ${item.why}` : item.note;
      el.title = item.note;
      el.classList.toggle('has-more', !!item.why);
      el.onclick = item.why ? () => bus.emit('ai:hunk', item) : null;
      break;
    }
    case 'too-large':
      q('.diff-banner-text').textContent = `Large diff (${plural(item.rows || 0, 'changed line')})`;
      q('button').onclick = () => item.onLoadLarge?.(item.path);
      break;
    case 'binary':
      q('.diff-banner-text').textContent = 'Binary file · not shown as diff';
      q('a').href = `api/v1/file/raw?path=${encodeURIComponent(item.path)}`;
      break;
    case 'error':
      q('.diff-banner-text').textContent = `Could not load this diff: ${item.message || 'unknown error'}`;
      q('button').onclick = () => item.onRetry?.(item.path);
      break;
    case 'image':
      renderImageDiffStage(el, item);
      break;
    case 'annotation':
      el.__item = item;
      mount(el, ...item.cards);
      break;
    default:
      updateDiffRow(el, item, layout, findingsCtx);
  }
}

const SEV_LETTER = { high: 'H', medium: 'M', low: 'L', nit: 'N' };

/** Style + wire a finding badge dot for one gutter position, or hide it when there's no finding there. */
function updateFindingDot(dot, path, line, side, findingsCtx) {
  if (!dot) return;
  const finding = line && findingsCtx ? findingsCtx.byKey.get(`${path}:${line}:${side}`) : null;
  if (!finding) { dot.hidden = true; dot.onclick = null; return; }
  dot.hidden = false;
  dot.className = `diff-finding-dot sev-${finding.severity}`;
  dot.textContent = SEV_LETTER[finding.severity] || '•';
  dot.title = finding.title;
  dot.onclick = (e) => { e.stopPropagation(); findingsCtx.onOpen?.(dot, finding); };
}

/** Coverage marker for an added line (Checks → Coverage → Show in diff), else ''. */
function covClass(findingsCtx, path, row) {
  const c = row?.t === 'add' && findingsCtx?.coverage?.get(path);
  if (!c) return '';
  return c.miss.has(row.n) ? ' cov-miss' : c.hit.has(row.n) ? ' cov-hit' : '';
}

/** One side of a split row: line number, sign and code (or an empty filler). */
function fillCell(cell, row, side, item, findingsCtx) {
  const [ln, sign, code, add, dot] = cell.children;
  const path = item.path;
  const gs = side === 'old' ? 'LEFT' : 'RIGHT';
  wireCommentBtn(add, item, row && (side === 'old' ? row.o : row.n), gs);
  if (!row) {
    cell.className = `diff-cell ${side} empty`;
    ln.textContent = '';
    sign.textContent = '';
    clearIntraline(code);
    code.textContent = '';
    updateFindingDot(dot, path, null, null, findingsCtx);
    return;
  }
  const num = side === 'old' ? row.o : row.n;
  cell.className = `diff-cell ${side} ${row.t}${side === 'new' ? covClass(findingsCtx, path, row) : ''}`;
  ln.textContent = num ? String(num) : '';
  sign.textContent = row.t === 'ctx' ? ' ' : (row.t === (side === 'old' ? 'del' : 'add') ? (side === 'old' ? '-' : '+') : '');
  setCode(code, row);
  applyIntraline(code, row.ch, side === 'new');
  updateFindingDot(dot, path, num, side === 'old' ? 'LEFT' : 'RIGHT', findingsCtx);
}

function updateDiffRow(el, item, layout, findingsCtx) {
  if (layout === 'split') {
    fillCell(el.children[0], item.oldRow, 'old', item, findingsCtx);
    fillCell(el.children[1], item.newRow, 'new', item, findingsCtx);
    return;
  }
  const { row } = item;
  const [oldLn, newLn, sign, code, add, dot] = el.children;
  el.className = `diff-row unified ${row.t}${covClass(findingsCtx, item.path, row)}`;
  wireCommentBtn(add, item, row.t === 'del' ? row.o : row.n, row.t === 'del' ? 'LEFT' : 'RIGHT');
  oldLn.textContent = row.o ? String(row.o) : '';
  newLn.textContent = row.n ? String(row.n) : '';
  sign.textContent = row.t === 'add' ? '+' : (row.t === 'del' ? '-' : ' ');
  setCode(code, row);
  applyIntraline(code, row.ch, row.t === 'add');
  const side = row.t === 'del' ? 'LEFT' : 'RIGHT';
  const num = row.t === 'del' ? row.o : row.n;
  updateFindingDot(dot, item.path, num, side, findingsCtx);
}

/** Gutter "+" of one line (hidden when the line has no number on that side). Shift extends the range. */
function wireCommentBtn(btn, item, line, side) {
  btn.hidden = !line || !item.onAddComment;
  btn.onclick = btn.hidden ? null : (e) => {
    e.stopPropagation();
    item.onAddComment(line, side, e.shiftKey);
  };
}

function renderImageDiffStage(el, item) {
  const stage = el.querySelector('.diff-image-stage');
  const modeBtns = [...el.querySelectorAll('.diff-img-mode')];
  const { before, after } = imageSides(item.diff, { baseRev: item.baseRev, target: item.target });
  // Added or deleted: one side only, so the comparison modes need both.
  const both = !!(before && after);
  let mode = both ? (el.__imgMode || 'side') : 'side';
  const img = (cls, src, alt) => (src ? h('img', { class: cls, src, alt }) : h('span', { class: 'diff-img-none faint' }, 'none'));

  function build() {
    for (const b of modeBtns) {
      b.classList.toggle('active', b.dataset.mode === mode);
      b.disabled = !both && b.dataset.mode !== 'side';
    }
    if (mode === 'side') {
      const col = (label, src, alt) => h('div', { class: 'diff-img-col' }, h('span', { class: 'diff-img-label' }, label), img(null, src, alt));
      mount(stage, h('div', { class: 'diff-img-side' },
        col(`Before (${item.baseLabel || 'base'})`, before, 'Before revision'),
        col('After (worktree)', after, 'After revision')));
      return;
    }
    // Swipe clips the after image at --swipe-pct; onion-skin fades it.
    const swipe = mode === 'swipe';
    const box = h('div', { class: `diff-img-${mode}-container` });
    const afterImg = img(`diff-img-${mode}-after`, after, 'After');
    const slider = h('input', {
      type: 'range',
      min: '0',
      max: swipe ? '100' : '1',
      step: swipe ? '1' : '0.01',
      value: swipe ? '50' : '0.5',
      class: `diff-img-${mode}-slider`,
      'aria-label': swipe ? 'Swipe comparison percentage' : 'Onion-skin opacity',
      on: {
        input: (e) => {
          if (swipe) box.style.setProperty('--swipe-pct', `${e.target.value}%`);
          else afterImg.style.opacity = e.target.value;
        },
      },
    });
    if (swipe) box.style.setProperty('--swipe-pct', '50%');
    else afterImg.style.opacity = '0.5';
    mount(box, img(`diff-img-${mode}-before`, before, 'Before'), afterImg, slider);
    mount(stage, box);
  }

  for (const b of modeBtns) b.onclick = () => { mode = el.__imgMode = b.dataset.mode; build(); };
  build();
}

/**
 * Main DiffView feature.
 */
export function createDiffView(host, { onOpen } = {}) {
  const el = h('div', { class: 'view diff diff-view', hidden: true, role: 'region', 'aria-label': 'Diff view' });
  const toolbar = h('div', { class: 'diff-toolbar' });
  const bannerSlot = h('div', { class: 'diff-banner-slot', hidden: true });
  const scroller = h('div', { class: 'diff-scroller', tabindex: '-1' });
  const sizer = h('div', { class: 'diff-sizer' });
  mount(scroller, sizer);
  mount(el, toolbar, bannerSlot, scroller);
  // Append: the views host also holds the editor's home and document views (mount() would
  // replace them and leave every file blank after the first diff).
  host.appendChild(el);

  let currentBase = 'HEAD';
  let currentTarget = 'worktree'; // a PR's headSha in review mode
  let ignoreWs = false;
  let layout = store.get('settings')?.['ui.diffLayout'] || 'split';
  let changeset = null;
  // Keyed by base + whitespace mode + path: a diff is only valid for the base it was taken against.
  const fileDiffs = new Map();
  const failed = new Map(); // same key -> error message (shown with Retry; never re-requested in a loop)
  const loadingDiffs = new Set();
  const diffKey = (path) => `${currentBase}\n${currentTarget}\n${ignoreWs ? 1 : 0}\n${path}`;
  let changesSeq = 0;
  const collapsed = new Set();
  let flatItems = [];
  let targetScrollPath = null;
  let activeFilePath = null;
  const viewedFiles = new Set();
  let hoverLine = null; // { path, line, side }: the diff line under the pointer (C / Alt+R)
  // Review cards survive rebuilds (a half-typed reply or composer must not reset when another
  // tab's drafts event lands); measured annotation heights too, so the list does not jump.
  const cardCache = new Map();
  const annHeights = new Map();
  // AI review findings (F4): keyed "path:line:side" -> Finding; onOpen renders the inline card.
  const findingsCtx = { byKey: new Map(), onOpen: null, coverage: null };
  let hunkAction = null; // { label, run(path, hunk) }: e.g. revert one hunk of an agent edit (F4)
  // AI change notes (explain.js) for one base/target pair: "path\nhunkId" -> { kind, note }, path -> summary.
  let notes = null; // { key, hunks: Map, files: Map }
  const notesFor = () => (notes && notes.key === `${currentBase}\n${currentTarget}` ? notes : null);

  // Pooled boxes hold one item's DOM; a box rebuilds only when its item type (or layout) changes.
  const vl = new VirtualList({
    scroller,
    sizer,
    rowHeight: 20,
    overscan: 15,
    create: () => h('div', { class: 'diff-item' }),
    update: (box, i) => {
      const item = flatItems[i];
      if (!item) return;
      const key = item.type === 'diff-row' ? `row:${layout}` : item.type;
      if (box.__key !== key) {
        for (const c of box.querySelectorAll('.diff-code')) clearIntraline(c);
        mount(box, createItem(item.type, layout));
        box.__key = key;
      }
      updateItem(box.firstChild, item, layout, findingsCtx);
      if (item.type === 'annotation') {
        // Re-observe: a pooled slot may hold a same-sized stack, and observe() always reports once.
        annRO.unobserve(box.firstChild);
        annRO.observe(box.firstChild);
      }
    },
  });
  // Annotation boxes start at an estimate; their real height feeds back into the height model.
  const annRO = new ResizeObserver((entries) => {
    let changed = false;
    for (const { target } of entries) {
      const item = target.__item;
      const hgt = Math.ceil(target.offsetHeight);
      if (!item || !hgt || !target.isConnected || Math.abs(hgt - item.estH) < 1) continue;
      item.estH = hgt;
      annHeights.set(item.key, hgt);
      changed = true;
    }
    if (changed) vl.setCount(flatItems.length);
  });
  vl.setHeights(heightModel((i) => itemHeight(flatItems[i])));
  scroller.__vl = vl;

  function setItems(items) {
    flatItems = items;
    vl.setCount(items.length); // re-measures and repaints every mounted item
  }

  const viewH = () => scroller.clientHeight || 800;
  const itemAt = (dy = 0) => vl.indexAt(vl.virtualTop(scroller.scrollTop, viewH()) + dy);
  /** Put item i at the top edge (file headers and hunks line up with the viewport). */
  function scrollToItem(i) {
    scroller.scrollTop = vl.scrollTopFor(vl.rowTop(i), viewH());
    vl.schedule();
  }

  // Toolbar
  const infoSpan = h('span', { class: 'diff-toolbar-info' });
  const layoutBtn = (name, label) => h('button', { class: `btn sm diff-layout-btn${layout === name ? ' active' : ''}`, 'aria-pressed': String(layout === name), 'data-layout': name, on: { click: () => setLayout(name) } }, label);
  const splitBtn = layoutBtn('split', 'Split');
  const unifiedBtn = layoutBtn('unified', 'Unified');
  const wsBtn = h('button', { class: 'btn sm diff-ws-btn', 'aria-pressed': 'false', 'aria-label': 'Whitespace', 'data-tip': 'Ignore whitespace-only changes', on: { click: toggleWs } },
    h('span', { class: 'lbl-ws' }, 'Whitespace'), h('span', { class: 'lbl-short', 'aria-hidden': 'true' }, '¶'));
  // Labels are dropped on a narrow toolbar (diff.css); the aria-label keeps the name.
  const checksBtn = h('button', { class: 'btn sm diff-checks-btn', hidden: !has('checks.breaking'), 'aria-label': 'Checks', 'data-tip': 'Breaking changes, tests, security, coverage for this diff', on: { click: () => bus.emit('checks:open') } }, icon('check-circle', 'sm'), h('span', { class: 'lbl' }, 'Checks'));
  const explainBtn = h('button', { class: 'btn sm diff-explain-btn', hidden: !has('ai.explain'), 'aria-label': 'Explain', 'data-tip': 'AI notes on what each change does', on: { click: () => bus.emit('ai:explain') } }, icon('sparkles', 'sm'), h('span', { class: 'lbl' }, 'Explain'));
  const closeBtn = h('button', {
    class: 'icon-btn sm diff-close-btn',
    'aria-label': 'Back to editor',
    'data-tip': 'Back to editor (Mod+D)',
    on: { click: () => hide() },
  }, icon('x', 'sm'));
  mount(toolbar,
    h('div', { class: 'diff-toolbar-left' }, infoSpan),
    h('div', { class: 'diff-toolbar-right' }, checksBtn, explainBtn, h('div', { class: 'btn-group' }, splitBtn, unifiedBtn), wsBtn, closeBtn));

  function setLayout(next) {
    layout = next;
    splitBtn.classList.toggle('active', layout === 'split');
    unifiedBtn.classList.toggle('active', layout === 'unified');
    splitBtn.setAttribute('aria-pressed', String(layout === 'split'));
    unifiedBtn.setAttribute('aria-pressed', String(layout === 'unified'));
    if (store.get('settings')?.['ui.diffLayout'] !== layout) {
      store.update('settings', (s) => ({ ...s, 'ui.diffLayout': layout }));
      request('settings', { method: 'PUT', query: { scope: 'user' }, body: { values: { 'ui.diffLayout': layout } } }).catch(() => {});
    }
    rebuildItems();
  }
  // The Settings dialog (or another window) changed the layout.
  store.subscribe('settings', (v) => {
    const want = v?.['ui.diffLayout'];
    if ((want === 'split' || want === 'unified') && want !== layout) setLayout(want);
  });

  async function toggleWs() {
    ignoreWs = !ignoreWs;
    wsBtn.classList.toggle('active', ignoreWs);
    wsBtn.setAttribute('aria-pressed', String(ignoreWs));
    await loadChanges(currentBase, targetScrollPath);
  }

  /** Forget cached diffs (all, or the given paths) so the next paint reads them fresh. */
  function invalidate(paths = null) {
    if (!paths) { fileDiffs.clear(); failed.clear(); return; }
    const hit = new Set(paths);
    for (const map of [fileDiffs, failed]) {
      for (const k of [...map.keys()]) if (hit.has(k.slice(k.lastIndexOf('\n') + 1))) map.delete(k);
    }
  }

  async function loadChanges(base = 'HEAD', targetPath = null) {
    currentBase = base;
    const my = ++changesSeq;
    const target = currentTarget;
    const label = (rev) => (rev === baseName?.base ? baseName.text : /^[0-9a-f]{40}$/i.test(rev) ? rev.slice(0, 7) : rev);
    infoSpan.textContent = `Loading diff for ${label(base)}…`;
    let resolveFirstPaint;
    el.__firstPaintPromise = new Promise((resolve) => { resolveFirstPaint = resolve; });
    try {
      const next = await request('git/changes', { query: { base, target } });
      if (my !== changesSeq) return; // a newer load (base switch, refresh) owns the view
      changeset = next;
      const tChangesArrived = performance.now();
      const stats = changeset.stats || { files: changeset.files?.length || 0, additions: 0, deletions: 0 };
      infoSpan.textContent = `${label(base)}${target === 'worktree' ? '' : ` → ${label(target)}`} · ${plural(stats.files, 'file')} changed · +${stats.additions} -${stats.deletions}`;

      const prev = vl.onPaint;
      vl.onPaint = (dur) => {
        try {
          prev?.(dur);
        } finally {
          vl.onPaint = prev;
          el.__firstPaintMs = performance.now() - tChangesArrived;
          resolveFirstPaint(el.__firstPaintMs);
        }
      };

      // Files past the first 10 collapse by default (FRONTEND.md § 6.12)
      if (!collapsed.size) {
        for (let i = 10; i < (changeset.files || []).length; i++) {
          const p = changeset.files[i].path;
          if (p !== targetPath) collapsed.add(p);
        }
      }
      if (targetPath) collapsed.delete(targetPath);

      // Preload diffs for all uncollapsed files
      const filesToLoad = (changeset.files || []).filter((f, i) => i < 10 || !collapsed.has(f.path));
      if (targetPath && !filesToLoad.some((f) => f.path === targetPath)) filesToLoad.push({ path: targetPath });
      await Promise.all(filesToLoad.map((f) => loadFileDiff(f.path)));
      if (my !== changesSeq) return;
      rebuildItems();
    } catch (e) {
      if (my === changesSeq) infoSpan.textContent = `Error loading changes: ${e.message}`;
    }
  }

  async function loadFileDiff(path, force = 0) {
    const key = diffKey(path);
    if (fileDiffs.has(key) && !force) return fileDiffs.get(key);
    try {
      const query = { path, base: currentBase, target: currentTarget, ignoreWs: ignoreWs ? 1 : 0 };
      if (force) query.force = 1;
      const diff = await request('git/diff', { query });
      fileDiffs.set(key, diff);
      failed.delete(key);
      return diff;
    } catch (e) {
      failed.set(key, e?.message || String(e));
      return null;
    }
  }

  /** Split layout: the k-th deleted row of a change block sits beside its k-th added row. */
  function pairHunkRows(hunk) {
    const rows = hunk.rows || [];
    const pairs = [];
    let i = 0;
    while (i < rows.length) {
      if (rows[i].t === 'ctx') {
        pairs.push({ oldRow: rows[i], newRow: rows[i] });
        i++;
        continue;
      }
      const dels = [];
      const adds = [];
      for (; i < rows.length && rows[i].t !== 'ctx'; i++) (rows[i].t === 'del' ? dels : adds).push(rows[i]);
      for (let k = 0; k < Math.max(dels.length, adds.length); k++) pairs.push({ oldRow: dels[k] || null, newRow: adds[k] || null });
    }
    return pairs;
  }

  /** Best-effort original text of lines on one side, for the "Suggest change" prefill. */
  function collectOriginalLines(path, from, to, side) {
    const out = [];
    for (const it of flatItems) {
      if (it.type !== 'diff-row' || it.path !== path) continue;
      const row = it.row ? (((it.row.t === 'del') === (side === 'LEFT')) ? it.row : null) : (side === 'LEFT' ? it.oldRow : it.newRow);
      const ln = row && (side === 'LEFT' ? row.o : row.n);
      if (ln >= from && ln <= to) out.push(row.text ?? '');
    }
    return out.join('\n');
  }

  /** Open the inline composer on a line (or a range: `extend` grows the open composer, `from` sets it). */
  function openComposerAt(path, line, side, extend = false, from = line) {
    if (!store.get('pr')) {
      toast({ kind: 'info', title: 'Comments need an open pull request', message: 'Open a PR to leave review comments.' });
      return;
    }
    const prev = getActiveComposer();
    if (extend && prev && !prev.draftId && prev.path === path && prev.side === side) {
      from = Math.min(prev.startLine || prev.line, line);
      line = Math.max(prev.line, line);
    }
    setActiveComposer({ path, line, startLine: from, side, originalLines: collectOriginalLines(path, from, line, side) });
  }

  /** The diff lines a text selection spans (one side: the column the selection starts in). */
  function selectionRange() {
    const sel = window.getSelection?.();
    if (!sel || sel.isCollapsed || !scroller.contains(sel.anchorNode)) return null;
    const at = (node) => {
      const el = node?.nodeType === 1 ? node : node?.parentElement;
      const box = el?.closest('.diff-item');
      const item = box && flatItems[box.__index];
      if (item?.type !== 'diff-row') return null;
      if (item.row) return { item, side: item.row.t === 'del' ? 'LEFT' : 'RIGHT', row: item.row };
      const left = !!el.closest('.diff-cell.old');
      return { item, side: left ? 'LEFT' : 'RIGHT', row: left ? item.oldRow : item.newRow };
    };
    const a = at(sel.anchorNode);
    const b = at(sel.focusNode);
    if (!a || !b || a.item.path !== b.item.path) return null;
    const num = (x) => x.row && (a.side === 'LEFT' ? x.row.o : x.row.n);
    const lines = [num(a), num(b)].filter(Boolean);
    if (!lines.length) return null;
    return { path: a.item.path, side: a.side, from: Math.min(...lines), to: Math.max(...lines) };
  }

  function cachedCard(key, sig, make) {
    const hit = cardCache.get(key);
    if (hit && hit.sig === sig) { hit.used = true; return hit.el; }
    const el = make();
    cardCache.set(key, { sig, el, used: true });
    return el;
  }

  /** Threads, drafts and the open composer anchored at one line and side, or null. */
  function annotationItemFor(path, line, side) {
    if (!line) return null;
    const at = (x) => x.path === path && x.side === side && (x.line ?? x.originalLine) === line;
    const threadsAt = getThreads().filter(at);
    const draftsAt = getDrafts().filter((d) => at(d) && !d.threadId);
    const ac = getActiveComposer();
    const composerHere = !!ac && at(ac);
    if (!threadsAt.length && !draftsAt.length && !composerHere) return null;
    const cards = [
      ...threadsAt.map((t) => cachedCard(`t:${t.id}`, `${t.comments?.length}:${t.outdated}:${t.resolved}`, () => createInlineThread(t))),
      ...draftsAt.map((d) => cachedCard(`d:${d.id}`, `${d.updatedAt}:${d.body}:${d.stale}`, () => createInlineDraft(d, {
        onEdit: (x) => setActiveComposer({ path: x.path, line: x.line, startLine: x.startLine || x.line, side: x.side, originalLines: '', draftId: x.id, initialBody: x.body }),
      }))),
    ];
    if (composerHere) cards.push(cachedCard('composer', ac, () => createInlineComposer({ ...ac })));
    const key = `${path}\n${side}\n${line}`;
    return { type: 'annotation', path, line, side, cards, key, estH: annHeights.get(key) || estimateAnnotationHeight(threadsAt, draftsAt, composerHere) };
  }

  function rebuildItems() {
    const items = [];
    const pr = store.get('pr');
    for (const c of cardCache.values()) c.used = false;
    for (const f of changeset?.files || []) {
      items.push({
        type: 'file-header',
        path: f.path,
        oldPath: f.oldPath,
        status: f.status,
        additions: f.additions,
        deletions: f.deletions,
        // PR mode: review/viewed is the source of truth (ViewedState objects, not booleans).
        viewed: pr ? !!(getViewedState()[f.path] || f.viewed)?.viewed : viewedFiles.has(f.path) || f.viewed === true,
        collapsed: collapsed.has(f.path),
        onToggleCollapse: (p) => {
          activeFilePath = p;
          if (collapsed.has(p)) collapsed.delete(p);
          else collapsed.add(p);
          rebuildItems(); // loads the diff on demand when it opens
        },
        onToggleViewed: (p, checked) => {
          activeFilePath = p;
          if (pr) { toggleFileViewed(p, checked); return; }
          if (checked) viewedFiles.add(p);
          else viewedFiles.delete(p);
          const it = flatItems.find((x) => x.type === 'file-header' && x.path === p);
          if (it) it.viewed = checked;
        },
        onOpen: (p) => {
          hide();
          onOpen?.(p, { preview: false, focus: true });
        },
      });
      const nts = notesFor();
      const summary = nts?.files.get(f.path);
      if (summary) items.push({ type: 'file-note', path: f.path, note: summary });
      if (collapsed.has(f.path)) continue;

      const key = diffKey(f.path);
      if (failed.has(key)) {
        items.push({ type: 'error', path: f.path, message: failed.get(key), onRetry: (p) => { failed.delete(diffKey(p)); rebuildItems(); } });
        continue;
      }
      const diff = fileDiffs.get(key);
      if (!diff) {
        if (!loadingDiffs.has(key)) {
          loadingDiffs.add(key);
          loadFileDiff(f.path).finally(() => {
            loadingDiffs.delete(key);
            rebuildItems(); // the diff, or the error banner: never a silent re-request
          });
        }
        continue;
      }

      const kind = diffKind(f.path, diff);
      if (kind === 'too-large') {
        items.push({ type: 'too-large', path: f.path, rows: (f.additions || 0) + (f.deletions || 0), onLoadLarge: async (p) => { await loadFileDiff(p, 1); rebuildItems(); } });
        continue;
      }
      if (kind === 'image' || kind === 'svg') {
        items.push({ type: 'image', path: f.path, diff, baseRev: changeset.baseSha || currentBase, baseLabel: currentBase, target: currentTarget });
        if (kind === 'image') continue;
      }
      if (kind === 'binary') {
        items.push({ type: 'binary', path: f.path });
        continue;
      }

      const hunks = diff.hunks || [];
      hunks.forEach((hunk, k) => {
        items.push({
          type: 'hunk-sep',
          path: f.path,
          hunk,
          canUp: !!hunkGap(hunks, k, 'up'),
          canDown: !!hunkGap(hunks, k, 'down'),
          onExpand: (dir) => expandContext(f.path, hunks, k, dir),
          action: hunkAction ? { label: hunkAction.label, run: () => hunkAction.run(f.path, hunk) } : null,
        });
        const note = nts?.hunks.get(`${f.path}\n${hunk.id}`);
        if (note) items.push({ ...note, type: 'hunk-note', path: f.path, target: currentTarget });
        const onAddComment = (line, side, extend) => openComposerAt(f.path, line, side, extend);
        const push = (row) => {
          items.push({ ...row, type: 'diff-row', path: f.path, onAddComment });
          // Context lines exist on both sides: a LEFT thread on one shows too.
          const oldRow = row.row ? row.row.t !== 'add' && row.row : row.oldRow;
          const newRow = row.row ? row.row.t !== 'del' && row.row : row.newRow;
          for (const [r, side] of [[oldRow, 'LEFT'], [newRow, 'RIGHT']]) {
            const a = r && annotationItemFor(f.path, side === 'LEFT' ? r.o : r.n, side);
            if (a) items.push(a);
          }
        };
        if (layout === 'split') for (const pair of pairHunkRows(hunk)) push(pair);
        else for (const row of hunk.rows) push({ row });
      });
    }
    // The open composer outlives a rebuild that skips its file (diff reloading after a `changes`
    // event, or the file collapsed): its half-typed text comes back with the diff.
    const ac = getActiveComposer();
    for (const [k, c] of cardCache) if (!c.used && !(k === 'composer' && c.sig === ac)) cardCache.delete(k);

    setItems(items);
    if (targetScrollPath) {
      const idx = flatItems.findIndex((it) => it.path === targetScrollPath);
      if (idx >= 0) scrollToItem(idx);
    }
  }

  /**
   * Expand context around hunk k with new-side lines (read at the diff's target):
   * 20 lines up or down, or the whole gap to the neighbouring hunks for 'all'.
   */
  async function expandContext(path, hunks, k, dir) {
    try {
      for (const d of dir === 'all' ? ['up', 'down'] : [dir]) {
        const gap = hunkGap(hunks, k, d);
        if (!gap) continue;
        let [from, to] = gap;
        if (dir !== 'all') {
          if (d === 'up') from = Math.max(from, to - 19);
          else to = Math.min(to, from + 19);
        }
        const count = Number.isFinite(to) ? to - from + 1 : 5000;
        const res = await request('git/blob/lines', { query: { rev: currentTarget, path, from, count, hl: 1 } });
        expandHunk(hunks[k], d, (res?.lines || []).filter((l) => l.n >= from && l.n <= to));
      }
      targetScrollPath = null; // keep the reader where they are
      rebuildItems();
    } catch (e) {
      toast({ kind: 'error', title: 'Could not expand context', message: e.message });
    }
  }

  // Keyboard navigation
  el.addEventListener('keydown', (e) => {
    if (e.target.closest?.('input, textarea, select, [contenteditable="true"]')) return;
    // Plain letters only: Cmd/Ctrl+C must still copy, Cmd+V paste.
    const plain = !e.metaKey && !e.ctrlKey && !e.altKey;
    const arrow = e.key === 'ArrowDown' ? 1 : e.key === 'ArrowUp' ? -1 : 0;
    if (e.altKey && arrow) {
      e.preventDefault();
      jump(e.shiftKey ? 'file-header' : 'hunk-sep', arrow);
    } else if (plain && (e.key === 'v' || e.key === 'V')) {
      e.preventDefault();
      toggleCurrentViewed();
    } else if ((plain && (e.key === 'c' || e.key === 'C')) || (e.altKey && !e.metaKey && !e.ctrlKey && e.code === 'KeyR')) {
      // Alt+R by key code: on macOS Option+R types "®".
      e.preventDefault();
      const sel = selectionRange();
      if (sel) openComposerAt(sel.path, sel.to, sel.side, false, sel.from);
      else if (hoverLine) openComposerAt(hoverLine.path, hoverLine.line, hoverLine.side);
      else toast({ kind: 'info', title: 'Comment', message: 'Point at a line or select lines, then press C or Alt+R.' });
    } else if ((e.metaKey || e.ctrlKey) && (e.key === 'd' || e.key === 'D')) {
      e.preventDefault();
      hide();
    }
  });

  /** Next/previous item of a type (hunks, or files with Shift) from the top of the view. */
  function jump(type, dir) {
    for (let i = itemAt() + dir; i >= 0 && i < flatItems.length; i += dir) {
      if (flatItems[i].type === type) { scrollToItem(i); return; }
    }
  }

  function toggleCurrentViewed() {
    let target = activeFilePath ? flatItems.findIndex((it) => it.type === 'file-header' && it.path === activeFilePath) : -1;
    if (target < 0) {
      for (let i = itemAt(viewH() / 2); i >= 0; i--) {
        if (flatItems[i]?.type === 'file-header') { target = i; break; }
      }
    }
    if (target < 0) return;
    const item = flatItems[target];
    activeFilePath = item.path;
    if (store.get('pr')) {
      toggleFileViewed(item.path, !item.viewed);
      return;
    }
    if (viewedFiles.has(item.path)) viewedFiles.delete(item.path);
    else viewedFiles.add(item.path);
    item.viewed = viewedFiles.has(item.path);
    activeFilePath = item.path;
    vl.refresh();
    toast({ kind: 'ok', title: item.viewed ? 'Marked viewed' : 'Unmarked viewed', message: item.path, timeout: 1200 });
  }

  let previouslyVisible = [];
  let baseName = null; // { base, text }: a caller's name for a base, e.g. an agent's snapshot
  async function show({ path, base, target, baseLabel } = {}) {
    if (baseLabel) baseName = { base, text: baseLabel };
    // Review mode diffs the PR head against its merge base (or a round's head: the interdiff);
    // re-targeting the open view without a base keeps the one the reader chose.
    const pr = store.get('pr');
    const prTarget = pr?.headSha || 'worktree';
    target = target ?? (pr ? prTarget : 'worktree');
    if (base === undefined) base = !pr || currentTarget === target ? currentBase : (pr.mergeBaseSha || pr.baseSha || 'HEAD');
    if (target !== currentTarget) collapsed.clear();
    currentTarget = target;
    el.classList.toggle('pr-mode', !!pr);
    activeFilePath = path || null;
    targetScrollPath = path || null;
    if (el.hidden) {
      // Opening (not re-targeting an open view): remember what to restore, and re-read diffs
      // (files may have changed while the view was closed).
      previouslyVisible = [];
      for (const child of host.children) {
        if (child !== el) {
          if (!child.hidden) previouslyVisible.push(child);
          child.hidden = true;
        }
      }
      invalidate();
      el.hidden = false;
    }
    await loadChanges(base, path);
    bus.emit('diff:shown', { base: currentBase, target: currentTarget });
    // Closed while loading: do not scroll or take focus back from whatever the reader moved to.
    if (el.hidden) return;
    if (targetScrollPath) {
      const idx = flatItems.findIndex((it) => it.path === targetScrollPath);
      if (idx >= 0) scrollToItem(idx);
    }
    // Later rebuilds (lazy loads, expanders, live refresh) keep the reader's position.
    targetScrollPath = null;
    scroller.focus();
  }

  // Live updates: files changing on disk reload their diffs; the Changes panel's base picker
  // re-targets an open view.
  let pendingPaths = new Set();
  let pendingAll = false;
  const refreshSoon = debounce(() => {
    if (el.hidden) return;
    invalidate(pendingAll ? null : [...pendingPaths]);
    pendingPaths = new Set();
    pendingAll = false;
    loadChanges(currentBase);
  }, 400);
  bus.on('ev:fs', (ev) => {
    if (el.hidden) return;
    if (ev?.overflow) pendingAll = true;
    for (const c of ev?.changes || []) pendingPaths.add(c.path);
    refreshSoon();
  });
  bus.on('git:base', (base) => { if (!el.hidden && base) show({ path: activeFilePath, base }); });

  // Pointer line for C / Alt+R (delegated: rows are pooled).
  scroller.addEventListener('mouseover', (e) => {
    const box = e.target.closest?.('.diff-item');
    const item = box && flatItems[box.__index];
    if (item?.type !== 'diff-row') return;
    let row = item.row;
    let side = row && row.t === 'del' ? 'LEFT' : 'RIGHT';
    if (!row) {
      side = e.target.closest('.diff-cell.old') ? 'LEFT' : 'RIGHT';
      row = side === 'LEFT' ? item.oldRow : item.newRow;
    }
    const line = row && (side === 'LEFT' ? row.o : row.n);
    if (line) hoverLine = { path: item.path, line, side };
  });

  // headMoved "Refresh" (review.js) re-reads PR metadata, then the diff follows the new head.
  bus.on('diff:refresh', () => {
    const pr = store.get('pr');
    if (el.hidden || !pr?.headSha) return;
    currentTarget = pr.headSha;
    invalidate();
    loadChanges(currentBase);
  });
  // Threads, drafts (this tab or another's `drafts` event), viewed state and the composer
  // redraw the inline annotations in place.
  for (const ev of ['review:threads:updated', 'review:drafts:updated', 'review:composer:changed', 'review:state:updated']) {
    bus.on(ev, () => { if (!el.hidden && changeset) rebuildItems(); });
  }

  function hide() {
    targetScrollPath = null;
    el.hidden = true;
    if (previouslyVisible.length) {
      for (const child of previouslyVisible) child.hidden = false;
    } else {
      const doc = host.querySelector('.doc');
      if (doc) doc.hidden = false;
    }
    bus.emit('diff:closed');
  }

  /**
   * Push AI review findings (F4) into the open diff: a gutter dot at each finding's line,
   * collapsed to a severity letter by default. Clicking one calls `handlers.onOpen(dot, finding)`
   * so the caller (ai.js) owns the card's content and actions; this view only owns the anchor.
   */
  function setFindings(findings, handlers = {}) {
    findingsCtx.byKey = new Map((findings || []).map((f) => [`${f.path}:${f.line}:${f.side}`, f]));
    findingsCtx.onOpen = handlers.onOpen || null;
    vl.refresh();
  }

  /** A button on every hunk header (null removes it); `run(path, hunk)` owns the effect. */
  function setHunkAction(action) {
    hunkAction = action;
    rebuildItems();
  }

  /** Coverage of added lines: Map path -> { hit: Set, miss: Set } (null clears); see checks.js. */
  function setCoverage(map) {
    findingsCtx.coverage = map;
    vl.refresh();
  }

  /** AI change notes for the open base/target (null clears); see explain.js. */
  function setNotes(next) {
    notes = next ? { key: `${currentBase}\n${currentTarget}`, ...next } : null;
    rebuildItems();
  }

  /** A strip between the toolbar and the diff (null clears it), e.g. an agent edit's summary. */
  function setBanner(node) {
    bannerSlot.hidden = !node;
    mount(bannerSlot, node);
  }

  /** Re-read the change set and every diff (after something rewrote the files). */
  function reload() {
    invalidate();
    return loadChanges(currentBase);
  }

  return {
    el, show, hide, vl, setFindings, setHunkAction, setBanner, setNotes, setCoverage, reload,
    getBase: () => currentBase,
    getTarget: () => currentTarget,
    getChangeset: () => changeset,
    getActivePath: () => activeFilePath,
  };
}

/**
 * Gutter marker attachment and inline mini-diff handler.
 */
export async function attachGutter(scroller, path, vl) {
  if (!scroller || !path) return;
  try {
    const gutterData = await request('git/gutter', { query: { path, base: 'HEAD' } });
    const addedSet = new Set(gutterData.added || []);
    const modSet = new Set(gutterData.modified || []);
    const delSet = new Set(gutterData.deleted || []);
    if (!addedSet.size && !modSet.size && !delSet.size) return;

    function paintGutterMarkers() {
      for (const row of vl.mounted.values()) {
        const ln = row.firstElementChild;
        if (!ln) continue;
        const lineNum = row.__index + 1;
        ln.classList.remove('gutter-add', 'gutter-mod', 'gutter-del');
        if (addedSet.has(lineNum)) ln.classList.add('gutter-add');
        else if (modSet.has(lineNum)) ln.classList.add('gutter-mod');
        else if (delSet.has(lineNum)) ln.classList.add('gutter-del');

        if (!ln.__gutterClick) {
          ln.__gutterClick = true;
          let lastTrigger = 0;
          const onTrigger = (e) => {
            const now = Date.now();
            if (now - lastTrigger < 300) return;
            lastTrigger = now;
            const currentLine = row.__index + 1;
            if (addedSet.has(currentLine) || modSet.has(currentLine) || delSet.has(currentLine)) {
              e.stopPropagation();
              e.preventDefault();
              openMiniDiff(row, path, currentLine);
            }
          };
          ln.addEventListener('click', onTrigger);
          ln.addEventListener('pointerdown', onTrigger);
        }
      }
    }

    const prevOnPaint = vl.onPaint;
    vl.onPaint = (dur) => {
      try {
        prevOnPaint?.(dur);
      } finally {
        paintGutterMarkers();
      }
    };

    const unsub = bus.on('git:refresh', async () => {
      if (!scroller.isConnected) { unsub(); return; }
      try {
        const fresh = await request('git/gutter', { query: { path, base: 'HEAD' } });
        addedSet.clear(); (fresh.added || []).forEach((n) => addedSet.add(n));
        modSet.clear(); (fresh.modified || []).forEach((n) => modSet.add(n));
        delSet.clear(); (fresh.deleted || []).forEach((n) => delSet.add(n));
        paintGutterMarkers();
      } catch {}
    });

    paintGutterMarkers();
  } catch {}
}

async function openMiniDiff(row, path, lineNum) {
  // A second click on the marker closes the card.
  const existing = row.parentNode?.querySelector('.mini-diff-card');
  if (existing) { existing.remove(); return; }

  try {
    const diff = await request('git/diff', { query: { path, base: 'HEAD' } });
    if (!diff?.hunks?.length) return;
    const targetHunk = diff.hunks.find((hk) => lineNum >= hk.newStart && lineNum <= hk.newStart + hk.newLines) || diff.hunks[0];

    const card = h('div', { class: 'mini-diff-card', role: 'dialog', 'aria-label': 'Mini diff' },
      h('div', { class: 'mini-diff-head' },
        h('span', { class: 'mini-diff-header' }, targetHunk.header),
        h('button', { class: 'icon-btn xs', 'aria-label': 'Close mini diff', on: { click: () => card.remove() } }, icon('x', 'xs'))),
      h('div', { class: 'mini-diff-rows' },
        targetHunk.rows.map((r) => {
          const code = h('span', { class: 'mini-diff-code' });
          setCode(code, r);
          return h('div', { class: `mini-diff-row ${r.t}` }, h('span', { class: 'mini-diff-sign' }, r.t === 'add' ? '+' : (r.t === 'del' ? '-' : ' ')), code);
        })));

    card.style.position = 'absolute';
    card.style.left = '60px';
    card.style.right = '20px';
    card.style.zIndex = '50';
    const match = row.style.transform?.match(/translateY\((\d+(?:\.\d+)?)px\)/);
    const top = match ? parseFloat(match[1]) + (row.offsetHeight || 20) : (lineNum * 20);
    card.style.transform = `translateY(${top}px)`;

    row.parentNode.insertBefore(card, row.nextSibling);
  } catch (e) {
    toast({ kind: 'error', title: 'Cannot load mini diff', message: e.message });
  }
}
