// Mock-only syntax highlighter. Produces the same HTML contract as the backend
// (escaped text + <span class="t-…">, docs/spec/API.md § 11) so the UI can be built and
// demoed before the backend ships highlight v2. Regex based, whole-file, cached by the caller.
import { escapeHtml } from '../core/dom.js';

const words = (s) => new Set(s.split(/\s+/).filter(Boolean));

const C_LIKE_OPS = /[+\-*/%=<>!&|^~?:]+/y;

const LANGS = {
  rust: {
    doc: [/\/\/[/!][^\n]*/y],
    comment: [/\/\/[^\n]*/y, /\/\*[\s\S]*?\*\//y],
    string: [/b?r(#*)"[\s\S]*?"\1/y, /b?"(?:[^"\\]|\\[\s\S])*"/y, /b?'(?:[^'\\\n]|\\.){1,10}'(?!\w)/y],
    keywords: words('as async await break const continue crate dyn else enum extern fn for if impl in let loop match mod move mut pub ref return self Self static struct super trait type union unsafe use where while yield'),
    constants: words('true false None Some Ok Err'),
    builtins: words('i8 i16 i32 i64 i128 isize u8 u16 u32 u64 u128 usize f32 f64 bool char str String Vec Option Result Box Rc Arc HashMap HashSet BTreeMap Path PathBuf'),
    defAfter: words('fn struct enum trait type mod union impl'),
    macro: /[a-z_][a-zA-Z0-9_]*!/y,
    attr: /#!?\[[^\]\n]*\]/y,
    lifetime: /'[a-z_]\w*/y,
    selfWords: words('self Self crate super'),
  },
  js: {
    doc: [/\/\*\*[\s\S]*?\*\//y],
    comment: [/\/\/[^\n]*/y, /\/\*[\s\S]*?\*\//y],
    string: [/`(?:[^`\\]|\\[\s\S])*`/y, /"(?:[^"\\\n]|\\.)*"/y, /'(?:[^'\\\n]|\\.)*'/y],
    regex: /\/(?![*/])(?:[^/\\\n[]|\\.|\[(?:[^\]\\\n]|\\.)*\])+\/[dgimsuyv]*/y,
    keywords: words('async await break case catch class const continue debugger default delete do else export extends finally for from function get if import in instanceof let new of return set static super switch this throw try typeof var void while with yield as interface type enum implements private protected public readonly declare namespace abstract keyof infer satisfies'),
    constants: words('true false null undefined NaN Infinity'),
    builtins: words('string number boolean any unknown never void object bigint symbol Array Map Set Promise Object String Number Boolean Error Date RegExp JSON Math console window document'),
    defAfter: words('function class interface type enum const let var'),
    decorator: /@[A-Za-z_$][\w$]*/y,
    selfWords: words('this super'),
  },
  python: {
    doc: [/"""[\s\S]*?"""/y, /'''[\s\S]*?'''/y],
    comment: [/#[^\n]*/y],
    string: [/[rbfu]{0,2}"(?:[^"\\\n]|\\.)*"/y, /[rbfu]{0,2}'(?:[^'\\\n]|\\.)*'/y],
    keywords: words('and as assert async await break class continue def del elif else except finally for from global if import in is lambda nonlocal not or pass raise return try while with yield match case'),
    constants: words('True False None'),
    builtins: words('int float str bool list dict set tuple bytes object type len range print isinstance super Exception'),
    defAfter: words('def class'),
    decorator: /@[A-Za-z_][\w.]*/y,
    selfWords: words('self cls'),
  },
  go: {
    doc: [],
    comment: [/\/\/[^\n]*/y, /\/\*[\s\S]*?\*\//y],
    string: [/`[^`]*`/y, /"(?:[^"\\\n]|\\.)*"/y, /'(?:[^'\\\n]|\\.)*'/y],
    keywords: words('break case chan const continue default defer else fallthrough for func go goto if import interface map package range return select struct switch type var'),
    constants: words('true false nil iota'),
    builtins: words('bool byte complex64 complex128 error float32 float64 int int8 int16 int32 int64 rune string uint uint8 uint16 uint32 uint64 uintptr any append cap close copy delete len make new panic print println recover'),
    defAfter: words('func type'),
  },
  clike: {
    doc: [/\/\*\*[\s\S]*?\*\//y],
    comment: [/\/\/[^\n]*/y, /\/\*[\s\S]*?\*\//y],
    string: [/"(?:[^"\\\n]|\\.)*"/y, /'(?:[^'\\\n]|\\.)*'/y],
    keywords: words('auto break case catch class const constexpr continue default delete do else enum explicit extern for friend goto if inline namespace new noexcept operator private protected public register return sizeof static struct switch template this throw try typedef typename union using virtual volatile while final override package import extends implements interface abstract synchronized instanceof var val fun when object data sealed'),
    constants: words('true false NULL nullptr null'),
    builtins: words('int char short long float double void bool unsigned signed size_t uint8_t uint16_t uint32_t uint64_t int32_t int64_t String Integer Boolean List Map'),
    defAfter: words('class struct enum interface fun namespace'),
    attr: /#\s*[a-z]+[^\n]*/y,
    decorator: /@[A-Za-z_][\w.]*/y,
    selfWords: words('this'),
  },
  shell: {
    doc: [],
    comment: [/#[^\n]*/y],
    string: [/"(?:[^"\\]|\\[\s\S])*"/y, /'[^']*'/y],
    keywords: words('if then else elif fi for while until do done case esac function in return local export readonly set unset shift exit source trap select'),
    constants: words('true false'),
    builtins: words('echo printf cd pwd test read eval exec mkdir rm cp mv ln cat grep sed awk curl git cargo tar install'),
    variable: /\$\{[^}\n]*\}|\$[A-Za-z_]\w*|\$[0-9@#?$!*-]/y,
    defAfter: words('function'),
  },
};
LANGS.ts = LANGS.js;

const EXT = {
  rs: 'rust', js: 'js', mjs: 'js', cjs: 'js', jsx: 'js', ts: 'ts', tsx: 'ts', mts: 'ts',
  py: 'python', go: 'go', c: 'clike', h: 'clike', cc: 'clike', cpp: 'clike', hpp: 'clike', java: 'clike', kt: 'clike', cs: 'clike', swift: 'clike',
  sh: 'shell', bash: 'shell', zsh: 'shell',
  toml: 'toml', yaml: 'yaml', yml: 'yaml', json: 'json', md: 'markdown', markdown: 'markdown', html: 'html', css: 'css', lock: 'toml',
};

export const LANGUAGE_NAMES = {
  rust: 'Rust', js: 'JavaScript', ts: 'TypeScript', python: 'Python', go: 'Go', clike: 'C-family', shell: 'Shell',
  toml: 'TOML', yaml: 'YAML', json: 'JSON', markdown: 'Markdown', html: 'HTML', css: 'CSS',
};

export function languageFor(path) {
  const name = path.split('/').pop().toLowerCase();
  if (name === 'dockerfile' || name === 'makefile') return 'shell';
  const ext = name.includes('.') ? name.split('.').pop() : '';
  return EXT[ext] || null;
}

// ---------- token stream → per-line HTML ----------
class Out {
  constructor() { this.lines = []; this.cur = ''; }
  push(text, cls) {
    const parts = text.split('\n');
    parts.forEach((part, i) => {
      if (i > 0) { this.lines.push(this.cur); this.cur = ''; }
      if (!part) return;
      const esc = escapeHtml(part);
      this.cur += cls ? `<span class="${cls}">${esc}</span>` : esc;
    });
  }
  done() { this.lines.push(this.cur); return this.lines; }
}

function tryAt(re, src, i) {
  re.lastIndex = i;
  const m = re.exec(src);
  return m && m.index === i ? m[0] : null;
}

function pushString(out, s) {
  // highlight escapes inside strings
  const re = /\\(?:x[0-9a-fA-F]{2}|u\{[0-9a-fA-F]+\}|u[0-9a-fA-F]{4}|[\s\S])/g;
  let pos = 0;
  let m;
  while ((m = re.exec(s))) {
    if (m.index > pos) out.push(s.slice(pos, m.index), 't-s');
    out.push(m[0], 't-se');
    pos = m.index + m[0].length;
  }
  if (pos < s.length) out.push(s.slice(pos), 't-s');
}

const IDENT = /[A-Za-z_$][\w$]*/y;
const FN_DEF = words('fn function def func');
const TYPE_DEF = words('struct enum trait type class interface union impl');
const NS_DEF = words('mod namespace package module');
const NUMBER = /(?:0[xX][\da-fA-F_]+|0[bB][01_]+|0[oO][0-7_]+|\d[\d_]*(?:\.\d[\d_]*)?(?:[eE][+-]?\d+)?)(?:[a-zA-Z_]\w*)?/y;
const PUNCT = /[{}()[\];,.]/y;

function highlightCode(src, L) {
  const out = new Out();
  let i = 0;
  let plain = '';
  let prevWord = '';
  let prevChar = '';
  const flushPlain = () => { if (plain) { out.push(plain, ''); plain = ''; } };
  const n = src.length;
  while (i < n) {
    const ch = src[i];
    if (ch === ' ' || ch === '\t' || ch === '\n' || ch === '\r') {
      plain += ch;
      i++;
      continue;
    }
    let m = null;
    // doc comments, comments
    for (const re of L.doc || []) if ((m = tryAt(re, src, i))) break;
    if (m) { flushPlain(); out.push(m, 't-cd'); i += m.length; prevChar = ''; continue; }
    for (const re of L.comment || []) if ((m = tryAt(re, src, i))) break;
    if (m) { flushPlain(); out.push(m, 't-c'); i += m.length; prevChar = ''; continue; }
    if (L.attr && (m = tryAt(L.attr, src, i))) { flushPlain(); out.push(m, 't-m'); i += m.length; continue; }
    if (L.decorator && (m = tryAt(L.decorator, src, i))) { flushPlain(); out.push(m, 't-m'); i += m.length; continue; }
    if (L.lifetime && ch === "'" && (m = tryAt(L.lifetime, src, i)) && src[i + m.length] !== "'") {
      flushPlain(); out.push(m, 't-m'); i += m.length; continue;
    }
    for (const re of L.string || []) if ((m = tryAt(re, src, i))) break;
    if (m) { flushPlain(); pushString(out, m); i += m.length; prevWord = ''; prevChar = '"'; continue; }
    if (L.variable && (m = tryAt(L.variable, src, i))) { flushPlain(); out.push(m, 't-v'); i += m.length; continue; }
    if (L.regex && ch === '/' && !/[\w)\]$]/.test(prevChar) && (m = tryAt(L.regex, src, i))) {
      flushPlain(); out.push(m, 't-re'); i += m.length; prevChar = '/'; continue;
    }
    if (/[0-9]/.test(ch) && !/[\w$]/.test(src[i - 1] || '') && (m = tryAt(NUMBER, src, i))) {
      flushPlain(); out.push(m, 't-n'); i += m.length; prevChar = '0'; continue;
    }
    if (L.macro && (m = tryAt(L.macro, src, i))) { flushPlain(); out.push(m, 't-m'); i += m.length; prevWord = ''; continue; }
    if ((m = tryAt(IDENT, src, i))) {
      flushPlain();
      const rest = src.slice(i + m.length, i + m.length + 64);
      const call = /^\s*(\(|::<|<[A-Za-z_][\w, ]*>\s*\()/.test(rest);
      const path = rest.startsWith('::');
      let cls = '';
      if (L.keywords.has(m)) cls = L.selfWords?.has(m) ? 't-vb' : 't-k';
      else if (L.constants.has(m)) cls = 't-b';
      else if (L.selfWords?.has(m)) cls = 't-vb';
      else if (FN_DEF.has(prevWord)) cls = 't-fd';
      else if (TYPE_DEF.has(prevWord)) cls = 't-t';
      else if (NS_DEF.has(prevWord)) cls = 't-ns';
      else if (L.builtins.has(m)) cls = call && /^[a-z]/.test(m) ? 't-f' : 't-tb';
      else if (path) cls = /^[A-Z]/.test(m) ? 't-t' : 't-ns';
      else if (call) cls = /^[A-Z]/.test(m) && prevChar !== '.' ? 't-t' : 't-f';
      else if (prevChar === '.') cls = 't-pr';
      else if (/^[A-Z][A-Z0-9_]+$/.test(m) && m.length > 1) cls = 't-b';
      else if (/^[A-Z]/.test(m)) cls = 't-t';
      out.push(m, cls);
      prevWord = m;
      prevChar = 'a';
      i += m.length;
      continue;
    }
    if ((m = tryAt(C_LIKE_OPS, src, i))) { flushPlain(); out.push(m, 't-o'); i += m.length; prevChar = m[m.length - 1]; prevWord = ''; continue; }
    if ((m = tryAt(PUNCT, src, i))) {
      flushPlain();
      out.push(m, 't-p');
      prevChar = m;
      if (m !== '.') prevWord = '';
      i += 1;
      continue;
    }
    plain += ch;
    prevChar = ch;
    i++;
  }
  flushPlain();
  return out.done();
}

// ---------- line-based languages ----------
function highlightLines(src, fn) {
  return src.split('\n').map((line) => {
    const out = new Out();
    fn(line, out);
    return out.done()[0] || '';
  });
}

function hlToml(line, out) {
  let m;
  if ((m = line.match(/^(\s*)(\[\[?[^\]]*\]\]?)(.*)$/))) { out.push(m[1], ''); out.push(m[2], 't-t'); hlValue(m[3], out); return; }
  if ((m = line.match(/^(\s*#.*)$/))) { out.push(m[1], 't-c'); return; }
  if ((m = line.match(/^(\s*)([\w.\-"]+)(\s*=\s*)(.*)$/))) { out.push(m[1], ''); out.push(m[2], 't-pr'); out.push(m[3], 't-o'); hlValue(m[4], out); return; }
  hlValue(line, out);
}
function hlYaml(line, out) {
  let m;
  if ((m = line.match(/^(\s*#.*)$/))) { out.push(m[1], 't-c'); return; }
  if ((m = line.match(/^(\s*-?\s*)([\w.\-"' ]+?)(:)(\s|$)(.*)$/))) { out.push(m[1], 't-p'); out.push(m[2], 't-pr'); out.push(m[3], 't-p'); out.push(m[4], ''); hlValue(m[5], out); return; }
  hlValue(line, out);
}
function hlValue(s, out) {
  const re = /("(?:[^"\\]|\\.)*"|'[^']*')|(#.*$)|\b(true|false|null|yes|no)\b|(-?\b\d[\d_.]*\b)|([{}[\],:])/g;
  let pos = 0;
  let m;
  while ((m = re.exec(s))) {
    if (m.index > pos) out.push(s.slice(pos, m.index), '');
    const cls = m[1] ? 't-s' : m[2] ? 't-c' : m[3] ? 't-b' : m[4] ? 't-n' : 't-p';
    if (m[1]) pushString(out, m[0]); else out.push(m[0], cls);
    pos = m.index + m[0].length;
  }
  if (pos < s.length) out.push(s.slice(pos), '');
}
function hlJson(line, out) {
  const re = /("(?:[^"\\]|\\.)*")(\s*:)?|\b(true|false|null)\b|(-?\d[\d.eE+-]*)|([{}[\],])/g;
  let pos = 0;
  let m;
  while ((m = re.exec(line))) {
    if (m.index > pos) out.push(line.slice(pos, m.index), '');
    if (m[1]) { if (m[2]) { out.push(m[1], 't-pr'); out.push(m[2], 't-p'); } else pushString(out, m[1]); }
    else out.push(m[0], m[3] ? 't-b' : m[4] ? 't-n' : 't-p');
    pos = m.index + m[0].length;
  }
  if (pos < line.length) out.push(line.slice(pos), '');
}

function highlightMarkdown(src) {
  let fence = false;
  return src.split('\n').map((line) => {
    const out = new Out();
    if (/^\s*(```|~~~)/.test(line)) { fence = !fence; out.push(line, 't-p'); return out.done()[0]; }
    if (fence) { out.push(line, 't-s'); return out.done()[0]; }
    let m;
    if ((m = line.match(/^(#{1,6}\s)(.*)$/))) { out.push(m[1], 't-p'); out.push(m[2], 't-h'); return out.done()[0]; }
    if ((m = line.match(/^(\s*>)(.*)$/))) { out.push(m[1], 't-p'); out.push(m[2], 't-c'); return out.done()[0]; }
    const lead = line.match(/^(\s*(?:[-*+]|\d+\.)\s)/);
    let rest = line;
    if (lead) { out.push(lead[1], 't-k'); rest = line.slice(lead[1].length); }
    const re = /(`[^`]+`)|(\*\*[^*]+\*\*|__[^_]+__)|(\*[^*\s][^*]*\*|_[^_\s][^_]*_)|(!?\[[^\]]*\]\([^)]*\))|(\|)/g;
    let pos = 0;
    while ((m = re.exec(rest))) {
      if (m.index > pos) out.push(rest.slice(pos, m.index), '');
      out.push(m[0], m[1] ? 't-s' : m[2] ? 't-st' : m[3] ? 't-em' : m[4] ? 't-l' : 't-p');
      pos = m.index + m[0].length;
    }
    if (pos < rest.length) out.push(rest.slice(pos), '');
    return out.done()[0] || '';
  });
}

function highlightHtml(src) {
  const out = new Out();
  const re = /(<!--[\s\S]*?-->)|(<\/?)([A-Za-z][\w-]*)|([\w-:@]+)(=)("[^"]*"|'[^']*')|(\/?>)|("[^"]*")/g;
  let pos = 0;
  let m;
  while ((m = re.exec(src))) {
    if (m.index > pos) out.push(src.slice(pos, m.index), '');
    if (m[1]) out.push(m[1], 't-c');
    else if (m[3]) { out.push(m[2], 't-p'); out.push(m[3], 't-tg'); }
    else if (m[4]) { out.push(m[4], 't-at'); out.push(m[5], 't-p'); out.push(m[6], 't-s'); }
    else if (m[7]) out.push(m[7], 't-p');
    else out.push(m[0], 't-s');
    pos = m.index + m[0].length;
  }
  if (pos < src.length) out.push(src.slice(pos), '');
  return out.done();
}

function highlightCss(src) {
  const out = new Out();
  const re = /(\/\*[\s\S]*?\*\/)|(--[\w-]+)|([\w-]+)(\s*:)(?![^{]*\{)|(#[0-9a-fA-F]{3,8}\b)|(-?\d*\.?\d+(?:px|em|rem|%|ms|s|vh|vw|deg|fr)?\b)|("[^"]*"|'[^']*')|([{}();,])|(@[\w-]+)/g;
  let pos = 0;
  let m;
  while ((m = re.exec(src))) {
    if (m.index > pos) out.push(src.slice(pos, m.index), '');
    if (m[1]) out.push(m[1], 't-c');
    else if (m[2]) out.push(m[2], 't-v');
    else if (m[3]) { out.push(m[3], 't-pr'); out.push(m[4], 't-p'); }
    else if (m[5]) out.push(m[5], 't-n');
    else if (m[6]) out.push(m[6], 't-n');
    else if (m[7]) out.push(m[7], 't-s');
    else if (m[8]) out.push(m[8], 't-p');
    else out.push(m[0], 't-k');
    pos = m.index + m[0].length;
  }
  if (pos < src.length) out.push(src.slice(pos), '');
  return out.done();
}

/** Highlight a whole file. Returns an array of per-line HTML strings. */
export function highlight(src, lang) {
  src = src.replace(/\r\n?/g, '\n');
  if (src.endsWith('\n')) src = src.slice(0, -1);
  switch (lang) {
    case 'toml': return highlightLines(src, hlToml);
    case 'yaml': return highlightLines(src, hlYaml);
    case 'json': return highlightLines(src, hlJson);
    case 'markdown': return highlightMarkdown(src);
    case 'html': return highlightHtml(src);
    case 'css': return highlightCss(src);
    default:
      if (LANGS[lang]) return highlightCode(src, LANGS[lang]);
      return src.split('\n').map((l) => escapeHtml(l));
  }
}
