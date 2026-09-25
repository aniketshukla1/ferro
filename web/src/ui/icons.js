// Inline SVG icons (24×24 stroke icons in the Lucide style), built with DOM APIs so
// they work under a strict CSP. Usage: icon('search'), icon('file-code', 'sm').
import { extname, basename } from '../core/util.js';

const NS = 'http://www.w3.org/2000/svg';
const p = (d) => ['path', { d }];
const c = (cx, cy, r) => ['circle', { cx, cy, r }];
const l = (x1, y1, x2, y2) => ['line', { x1, y1, x2, y2 }];
const r = (x, y, w, hh, rx = 2) => ['rect', { x, y, width: w, height: hh, rx }];
const FILE = p('M15 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V7Z');
const FOLD = p('M14 2v4a2 2 0 0 0 2 2h4');

const ICONS = {
  search: [c(11, 11, 8), p('m21 21-4.3-4.3')],
  x: [p('M18 6 6 18'), p('m6 6 12 12')],
  'chevron-right': [p('m9 18 6-6-6-6')],
  'chevron-down': [p('m6 9 6 6 6-6')],
  'chevron-left': [p('m15 18-6-6 6-6')],
  file: [FILE, FOLD],
  'file-code': [FILE, FOLD, p('M10 12.5 8 15l2 2.5'), p('m14 12.5 2 2.5-2 2.5')],
  'file-text': [FILE, FOLD, p('M10 9H8'), p('M16 13H8'), p('M16 17H8')],
  'file-json': [FILE, FOLD, p('M10 12a1 1 0 0 0-1 1v1a1 1 0 0 1-1 1 1 1 0 0 1 1 1v1a1 1 0 0 0 1 1'), p('M14 18a1 1 0 0 0 1-1v-1a1 1 0 0 1 1-1 1 1 0 0 1-1-1v-1a1 1 0 0 0-1-1')],
  'file-lock': [FILE, FOLD, r(9, 13, 6, 5, 1), p('M10 13v-1.5a2 2 0 0 1 4 0V13')],
  'file-image': [FILE, FOLD, c(10, 12, 2), p('m20 17-1.296-1.296a2.41 2.41 0 0 0-3.408 0L9 22')],
  folder: [p('M20 20a2 2 0 0 0 2-2V8a2 2 0 0 0-2-2h-7.9a2 2 0 0 1-1.69-.9L9.6 3.9A2 2 0 0 0 7.93 3H4a2 2 0 0 0-2 2v13a2 2 0 0 0 2 2Z')],
  'folder-open': [p('m6 14 1.5-2.9A2 2 0 0 1 9.24 10H20a2 2 0 0 1 1.94 2.5l-1.54 6a2 2 0 0 1-1.95 1.5H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h3.9a2 2 0 0 1 1.69.9l.81 1.2a2 2 0 0 0 1.67.9H18a2 2 0 0 1 2 2v2')],
  files: [p('M20 7h-3a2 2 0 0 1-2-2V2'), p('M9 18a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h7l4 4v10a2 2 0 0 1-2 2Z'), p('M3 7.6v12.8A1.6 1.6 0 0 0 4.6 22h9.8')],
  'git-branch': [l(6, 3, 6, 15), c(18, 6, 3), c(6, 18, 3), p('M18 9a9 9 0 0 1-9 9')],
  'git-pull-request': [c(18, 18, 3), c(6, 6, 3), p('M13 6h3a2 2 0 0 1 2 2v7'), l(6, 9, 6, 21)],
  'git-commit': [c(12, 12, 3), l(3, 12, 9, 12), l(15, 12, 21, 12)],
  'git-compare': [c(18, 18, 3), c(6, 6, 3), p('M13 6h3a2 2 0 0 1 2 2v7'), p('M11 18H8a2 2 0 0 1-2-2V9')],
  sparkles: [p('M9.937 15.5A2 2 0 0 0 8.5 14.063l-6.135-1.582a.5.5 0 0 1 0-.962L8.5 9.936A2 2 0 0 0 9.937 8.5l1.582-6.135a.5.5 0 0 1 .963 0L14.063 8.5A2 2 0 0 0 15.5 9.937l6.135 1.581a.5.5 0 0 1 0 .964L15.5 14.063a2 2 0 0 0-1.437 1.437l-1.582 6.135a.5.5 0 0 1-.963 0z'), p('M20 3v4'), p('M22 5h-4'), p('M4 17v2'), p('M5 18H3')],
  sliders: [p('M20 7h-9'), p('M14 17H5'), c(17, 17, 3), c(7, 7, 3)],
  settings: [p('M12.22 2h-.44a2 2 0 0 0-2 2v.18a2 2 0 0 1-1 1.73l-.43.25a2 2 0 0 1-2 0l-.15-.08a2 2 0 0 0-2.73.73l-.22.38a2 2 0 0 0 .73 2.73l.15.1a2 2 0 0 1 1 1.72v.51a2 2 0 0 1-1 1.74l-.15.09a2 2 0 0 0-.73 2.73l.22.38a2 2 0 0 0 2.73.73l.15-.08a2 2 0 0 1 2 0l.43.25a2 2 0 0 1 1 1.73V20a2 2 0 0 0 2 2h.44a2 2 0 0 0 2-2v-.18a2 2 0 0 1 1-1.73l.43-.25a2 2 0 0 1 2 0l.15.08a2 2 0 0 0 2.73-.73l.22-.39a2 2 0 0 0-.73-2.73l-.15-.08a2 2 0 0 1-1-1.74v-.5a2 2 0 0 1 1-1.74l.15-.09a2 2 0 0 0 .73-2.73l-.22-.38a2 2 0 0 0-2.73-.73l-.15.08a2 2 0 0 1-2 0l-.43-.25a2 2 0 0 1-1-1.73V4a2 2 0 0 0-2-2z'), c(12, 12, 3)],
  'list-tree': [p('M21 12h-8'), p('M21 6H8'), p('M21 18h-8'), p('M3 6v4c0 1.1.9 2 2 2h3'), p('M3 10v6c0 1.1.9 2 2 2h3')],
  'panel-left': [r(3, 3, 18, 18), p('M9 3v18')],
  'panel-right': [r(3, 3, 18, 18), p('M15 3v18')],
  sun: [c(12, 12, 4), p('M12 2v2'), p('M12 20v2'), p('m4.93 4.93 1.41 1.41'), p('m17.66 17.66 1.41 1.41'), p('M2 12h2'), p('M20 12h2'), p('m6.34 17.66-1.41 1.41'), p('m19.07 4.93-1.41 1.41')],
  moon: [p('M12 3a6 6 0 0 0 9 9 9 9 0 1 1-9-9Z')],
  contrast: [c(12, 12, 10), p('M12 18a6 6 0 0 0 0-12v12z')],
  command: [p('M15 6v12a3 3 0 1 0 3-3H6a3 3 0 1 0 3 3V6a3 3 0 1 0-3 3h12a3 3 0 1 0-3-3')],
  keyboard: [r(2, 4, 20, 16), p('M6 8h.01'), p('M10 8h.01'), p('M14 8h.01'), p('M18 8h.01'), p('M8 12h.01'), p('M12 12h.01'), p('M16 12h.01'), p('M7 16h10')],
  eye: [p('M2.062 12.348a1 1 0 0 1 0-.696 10.75 10.75 0 0 1 19.876 0 1 1 0 0 1 0 .696 10.75 10.75 0 0 1-19.876 0'), c(12, 12, 3)],
  message: [p('M21 15a2 2 0 0 1-2 2H7l-4 4V5a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2z')],
  check: [p('M20 6 9 17l-5-5')],
  'check-circle': [c(12, 12, 10), p('m9 12 2 2 4-4')],
  alert: [p('m21.73 18-8-14a2 2 0 0 0-3.48 0l-8 14A2 2 0 0 0 4 21h16a2 2 0 0 0 1.73-3'), p('M12 9v4'), p('M12 17h.01')],
  info: [c(12, 12, 10), p('M12 16v-4'), p('M12 8h.01')],
  refresh: [p('M3 12a9 9 0 0 1 9-9 9.75 9.75 0 0 1 6.74 2.74L21 8'), p('M21 3v5h-5'), p('M21 12a9 9 0 0 1-9 9 9.75 9.75 0 0 1-6.74-2.74L3 16'), p('M8 16H3v5')],
  zap: [p('M4 14a1 1 0 0 1-.78-1.63l9.9-10.2a.5.5 0 0 1 .86.46l-1.92 6.02A1 1 0 0 0 13 10h7a1 1 0 0 1 .78 1.63l-9.9 10.2a.5.5 0 0 1-.86-.46l1.92-6.02A1 1 0 0 0 11 14z')],
  copy: [r(8, 8, 14, 14), p('M4 16c-1.1 0-2-.9-2-2V4c0-1.1.9-2 2-2h10c1.1 0 2 .9 2 2')],
  external: [p('M15 3h6v6'), p('M10 14 21 3'), p('M18 13v6a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h6')],
  more: [c(12, 12, 1), c(19, 12, 1), c(5, 12, 1)],
  plus: [p('M5 12h14'), p('M12 5v14')],
  minus: [p('M5 12h14')],
  clock: [c(12, 12, 10), ['polyline', { points: '12 6 12 12 16 14' }]],
  history: [p('M3 12a9 9 0 1 0 9-9 9.75 9.75 0 0 0-6.74 2.74L3 8'), p('M3 3v5h5'), p('M12 7v5l4 2')],
  terminal: [['polyline', { points: '4 17 10 11 4 5' }], l(12, 19, 20, 19)],
  activity: [p('M22 12h-2.48a2 2 0 0 0-1.93 1.46l-2.35 8.36a.25.25 0 0 1-.48 0L9.24 2.18a.25.25 0 0 0-.48 0l-2.35 8.36A2 2 0 0 1 4.49 12H2')],
  cpu: [r(4, 4, 16, 16), r(9, 9, 6, 6, 1), p('M15 2v2'), p('M15 20v2'), p('M2 15h2'), p('M2 9h2'), p('M20 15h2'), p('M20 9h2'), p('M9 2v2'), p('M9 20v2')],
  lock: [r(3, 11, 18, 11), p('M7 11V7a5 5 0 0 1 10 0v4')],
  help: [c(12, 12, 10), p('M9.09 9a3 3 0 0 1 5.83 1c0 2-3 3-3 3'), p('M12 17h.01')],
  hash: [l(4, 9, 20, 9), l(4, 15, 20, 15), l(10, 3, 8, 21), l(16, 3, 14, 21)],
  at: [c(12, 12, 4), p('M16 8v5a3 3 0 0 0 6 0v-1a10 10 0 1 0-4 8')],
  enter: [['polyline', { points: '9 10 4 15 9 20' }], p('M20 4v7a4 4 0 0 1-4 4H4')],
  'arrow-up': [p('m5 12 7-7 7 7'), p('M12 19V5')],
  'arrow-down': [p('M12 5v14'), p('m19 12-7 7-7-7')],
  'corner-up': [p('M9 14 4 9l5-5'), p('M4 9h10.5a5.5 5.5 0 0 1 5.5 5.5 5.5 5.5 0 0 1-5.5 5.5H11')],
  'collapse-all': [r(3, 3, 18, 18), p('M8 12h8')],
  'expand-all': [r(3, 3, 18, 18), p('M8 12h8'), p('M12 8v8')],
  database: [['ellipse', { cx: 12, cy: 5, rx: 9, ry: 3 }], p('M3 5V19A9 3 0 0 0 21 19V5'), p('M3 12A9 3 0 0 0 21 12')],
  gauge: [p('m12 14 4-4'), p('M3.34 19a10 10 0 1 1 17.32 0')],
  wifi: [p('M12 20h.01'), p('M2 8.82a15 15 0 0 1 20 0'), p('M5 12.859a10 10 0 0 1 14 0'), p('M8.5 16.429a5 5 0 0 1 7 0')],
  'wifi-off': [p('M12 20h.01'), p('M8.5 16.429a5 5 0 0 1 7 0'), p('M5 12.859a10 10 0 0 1 5.17-2.69'), p('M19 12.859a10 10 0 0 0-2.007-1.523'), p('M2 8.82a15 15 0 0 1 4.177-2.643'), p('M22 8.82a15 15 0 0 0-11.288-3.764'), p('m2 2 20 20')],
  dot: [c(12, 12, 4)],
  circle: [c(12, 12, 9)],
  'circle-dot': [c(12, 12, 9), c(12, 12, 1)],
  layers: [p('m12.83 2.18a2 2 0 0 0-1.66 0L2.6 6.08a1 1 0 0 0 0 1.83l8.58 3.91a2 2 0 0 0 1.66 0l8.58-3.9a1 1 0 0 0 0-1.83Z'), p('m22 17.65-9.17 4.16a2 2 0 0 1-1.66 0L2 17.65'), p('m22 12.65-9.17 4.16a2 2 0 0 1-1.66 0L2 12.65')],
  braces: [p('M8 3H7a2 2 0 0 0-2 2v5a2 2 0 0 1-2 2 2 2 0 0 1 2 2v5c0 1.1.9 2 2 2h1'), p('M16 21h1a2 2 0 0 0 2-2v-5c0-1.1.9-2 2-2a2 2 0 0 1-2-2V5a2 2 0 0 0-2-2h-1')],
  mouse: [r(5, 2, 14, 20, 7), p('M12 6v4')],
};

/**
 * @param {string} name
 * @param {string} [cls]  size modifier: 'xs' | 'sm' | 'lg' | 'xl' (+ any extra classes)
 * @returns {SVGSVGElement}
 */
export function icon(name, cls = '') {
  const svg = document.createElementNS(NS, 'svg');
  svg.setAttribute('viewBox', '0 0 24 24');
  svg.setAttribute('class', cls ? `i ${cls}` : 'i');
  svg.setAttribute('aria-hidden', 'true');
  svg.setAttribute('focusable', 'false');
  for (const [tag, attrs] of ICONS[name] || ICONS.dot) {
    const el = document.createElementNS(NS, tag);
    for (const k in attrs) el.setAttribute(k, attrs[k]);
    svg.appendChild(el);
  }
  return svg;
}

/** The ferro mark: an "F" cut from three bars. */
export function markGlyph() {
  const svg = document.createElementNS(NS, 'svg');
  svg.setAttribute('viewBox', '0 0 24 24');
  svg.setAttribute('aria-hidden', 'true');
  for (const [x, y, w, hh] of [[6, 3.5, 4, 17], [6, 3.5, 13, 4], [6, 10.6, 10, 3.6]]) {
    const el = document.createElementNS(NS, 'rect');
    el.setAttribute('x', x);
    el.setAttribute('y', y);
    el.setAttribute('width', w);
    el.setAttribute('height', hh);
    el.setAttribute('rx', 1.1);
    svg.appendChild(el);
  }
  return svg;
}

/** The tagline, in one place (docs/BRAND.md). */
export const TAGLINE = 'Iron-clad code review.';

/** A <span class="mark"> brand mark: the forge square (ember gradient, white F; docs/BRAND.md). */
export function brandMark(cls = '') {
  const span = document.createElement('span');
  span.className = `mark ${cls}`.trim();
  span.appendChild(markGlyph());
  return span;
}

// ---------- file-type icons ----------
const LANG = {
  rs: ['file-code', '#e2825a', 'Rust'],
  ts: ['file-code', '#5b9bd5', 'TypeScript'], tsx: ['file-code', '#5b9bd5', 'TSX'], mts: ['file-code', '#5b9bd5', 'TypeScript'],
  js: ['file-code', '#e5c25a', 'JavaScript'], jsx: ['file-code', '#e5c25a', 'JSX'], mjs: ['file-code', '#e5c25a', 'JavaScript'], cjs: ['file-code', '#e5c25a', 'JavaScript'],
  py: ['file-code', '#5a9fd4', 'Python'], go: ['file-code', '#5cc3d4', 'Go'],
  c: ['file-code', '#7f9cd6', 'C'], h: ['file-code', '#a88ee0', 'C header'], cc: ['file-code', '#6d8bd6', 'C++'], cpp: ['file-code', '#6d8bd6', 'C++'], hpp: ['file-code', '#a88ee0', 'C++ header'],
  java: ['file-code', '#d99a5b', 'Java'], kt: ['file-code', '#a07ae0', 'Kotlin'], swift: ['file-code', '#f07a4a', 'Swift'],
  rb: ['file-code', '#d0525a', 'Ruby'], php: ['file-code', '#8c93c9', 'PHP'], cs: ['file-code', '#7cc47f', 'C#'],
  sh: ['terminal', '#7cc47f', 'Shell'], bash: ['terminal', '#7cc47f', 'Shell'], zsh: ['terminal', '#7cc47f', 'Shell'],
  html: ['file-code', '#e3734b', 'HTML'], css: ['file-code', '#6b8cd9', 'CSS'], scss: ['file-code', '#d96ba0', 'SCSS'],
  json: ['file-json', '#d9b454', 'JSON'], toml: ['file-json', '#9aa3ad', 'TOML'], yaml: ['file-json', '#c97b7b', 'YAML'], yml: ['file-json', '#c97b7b', 'YAML'],
  md: ['file-text', '#6fa8dc', 'Markdown'], markdown: ['file-text', '#6fa8dc', 'Markdown'], txt: ['file-text', '#9aa3ad', 'Text'],
  sql: ['database', '#d9a45b', 'SQL'],
  png: ['file-image', '#b58ed9', 'PNG'], jpg: ['file-image', '#b58ed9', 'JPEG'], jpeg: ['file-image', '#b58ed9', 'JPEG'], gif: ['file-image', '#b58ed9', 'GIF'], svg: ['file-image', '#e3a14b', 'SVG'], webp: ['file-image', '#b58ed9', 'WebP'], ico: ['file-image', '#b58ed9', 'Icon'], icns: ['file-image', '#b58ed9', 'Icon'],
  lock: ['file-lock', '#9aa3ad', 'Lockfile'],
};
const NAMES = {
  dockerfile: ['file-code', '#5aa0d9', 'Dockerfile'], makefile: ['terminal', '#9aa3ad', 'Makefile'],
  license: ['file-text', '#d9c35a', 'License'], 'cargo.lock': ['file-lock', '#9aa3ad', 'Lockfile'],
  '.gitignore': ['file', '#9aa3ad', 'Ignore'], 'readme.md': ['file-text', '#6fa8dc', 'Markdown'],
};

/** @returns {{icon:string, color:string, language:string}} */
export function fileKind(path) {
  const name = basename(path).toLowerCase();
  const hit = NAMES[name] || LANG[extname(path)];
  if (hit) return { icon: hit[0], color: hit[1], language: hit[2] };
  return { icon: 'file', color: '', language: '' };
}

/** A tinted file-type icon wrapped in <span class="ficon">. */
export function fileIcon(path, cls = 'sm') {
  const k = fileKind(path);
  const span = document.createElement('span');
  span.className = 'ficon';
  if (k.color) span.style.setProperty('--lc', k.color);
  span.appendChild(icon(k.icon, cls));
  return span;
}

export function folderIcon(open, cls = 'sm') {
  const span = document.createElement('span');
  span.className = 'ficon folder';
  span.appendChild(icon(open ? 'folder-open' : 'folder', cls));
  return span;
}
