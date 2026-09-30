// Mock of GET /file/markdown: a small CommonMark subset rendered to the same sanitized shape the
// backend produces (API.md § 5.4): heading ids, data-line on blocks, data-path for workspace links,
// data-ext-src (no src) for external images, highlighted code fences. Every text run is escaped.
import { highlight } from './hl.js';

const esc = (s) => s.replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
const slug = (s) => s.toLowerCase().replace(/<[^>]*>/g, '').replace(/[^\p{L}\p{N}\s-]/gu, '').trim().replace(/\s/g, '-');

function joinPath(base, rel) {
  const out = base ? base.split('/') : [];
  for (const seg of rel.split('/')) {
    if (!seg || seg === '.') continue;
    if (seg === '..') out.pop();
    else out.push(seg);
  }
  return out.join('/');
}

function inline(text, ctx) {
  const codes = [];
  let s = text.replace(/`([^`]+)`/g, (_, c) => `\u0000${codes.push(c) - 1}\u0000`);
  s = esc(s);
  s = s.replace(/!\[([^\]]*)\]\(([^)\s]+)\)/g, (_, alt, url) => {
    const u = url.replace(/&amp;/g, '&');
    if (/^https?:\/\//i.test(u)) { ctx.external++; return `<img alt="${alt}" data-ext-src="${esc(u)}">`; }
    return `<img alt="${alt}" src="${esc(ctx.rawUrl(joinPath(ctx.dir, u)))}">`;
  });
  s = s.replace(/\[([^\]]+)\]\(([^)\s]+)\)/g, (_, label, url) => {
    const u = url.replace(/&amp;/g, '&');
    if (/^https?:\/\//i.test(u)) return `<a href="${esc(u)}" rel="noopener noreferrer">${label}</a>`;
    if (u.startsWith('#')) return `<a href="${esc(u)}">${label}</a>`;
    const [p, frag] = u.split('#');
    const line = /^L(\d+)/.exec(frag || '')?.[1];
    return `<a href="#" data-path="${esc(joinPath(ctx.dir, p))}"${line ? ` data-line="${line}"` : ''} rel="noopener noreferrer">${label}</a>`;
  });
  s = s.replace(/\*\*([^*]+)\*\*/g, '<strong>$1</strong>').replace(/(^|[^*\w])\*([^*]+)\*/g, '$1<em>$2</em>').replace(/(^|\W)_([^_]+)_(?=\W|$)/g, '$1<em>$2</em>');
  return s.replace(/\u0000(\d+)\u0000/g, (_, i) => `<code>${esc(codes[i])}</code>`);
}

export function renderMarkdown(src, { path, rawUrl }) {
  const ctx = { dir: path.includes('/') ? path.slice(0, path.lastIndexOf('/')) : '', rawUrl, external: 0 };
  const lines = src.replace(/\r\n?/g, '\n').split('\n');
  const out = [];
  const headings = [];
  let i = 0;
  const isTableSep = (l) => /^\s*\|?\s*:?-{2,}:?\s*(\|\s*:?-{2,}:?\s*)*\|?\s*$/.test(l);
  const cells = (l) => l.trim().replace(/^\||\|$/g, '').split('|').map((c) => inline(c.trim(), ctx));
  while (i < lines.length) {
    const line = lines[i];
    const n = i + 1;
    let m;
    if (!line.trim()) { i++; continue; }
    if ((m = /^\s*(```+|~~~+)\s*([\w+-]*)/.exec(line))) {
      const fence = m[1];
      const lang = m[2];
      const body = [];
      i++;
      while (i < lines.length && !lines[i].trim().startsWith(fence)) body.push(lines[i++]);
      i++;
      const html = body.length ? highlight(`${body.join('\n')}\n`, lang || 'text').slice(0, body.length).join('\n') : '';
      out.push(`<pre data-line="${n}"><code${lang ? ` class="language-${esc(lang)}"` : ''}>${html}\n</code></pre>`);
      continue;
    }
    if ((m = /^(#{1,6})\s+(.*?)\s*#*$/.exec(line))) {
      const level = m[1].length;
      const text = inline(m[2], ctx);
      let id = slug(m[2]) || `h-${n}`;
      while (headings.some((x) => x.id === id)) id += '-1';
      headings.push({ id, level, line: n, text: m[2].replace(/[`*_]/g, '') });
      out.push(`<h${level} id="${id}" data-line="${n}">${text}</h${level}>`);
      i++;
      continue;
    }
    if (/^\s*([-*_])(\s*\1){2,}\s*$/.test(line)) { out.push(`<hr data-line="${n}">`); i++; continue; }
    if (line.includes('|') && isTableSep(lines[i + 1] || '')) {
      const head = cells(line);
      i += 2;
      const rows = [];
      while (i < lines.length && lines[i].includes('|') && lines[i].trim()) rows.push(cells(lines[i++]));
      out.push(`<table data-line="${n}"><thead><tr>${head.map((c) => `<th>${c}</th>`).join('')}</tr></thead><tbody>${rows.map((r) => `<tr>${r.map((c) => `<td>${c}</td>`).join('')}</tr>`).join('')}</tbody></table>`);
      continue;
    }
    if (/^\s*>/.test(line)) {
      const body = [];
      while (i < lines.length && /^\s*>/.test(lines[i])) body.push(lines[i++].replace(/^\s*>\s?/, ''));
      out.push(`<blockquote data-line="${n}"><p>${inline(body.join(' '), ctx)}</p></blockquote>`);
      continue;
    }
    if ((m = /^\s*([-*+]|\d+[.)])\s+/.exec(line))) {
      const ordered = /\d/.test(m[1]);
      const items = [];
      while (i < lines.length && (m = /^\s*([-*+]|\d+[.)])\s+(.*)$/.exec(lines[i]))) {
        let text = m[2];
        let box = '';
        const t = /^\[([ xX])\]\s+(.*)$/.exec(text);
        if (t) { box = `<input type="checkbox" disabled${t[1] === ' ' ? '' : ' checked'}> `; text = t[2]; }
        items.push(`<li data-line="${i + 1}">${box}${inline(text, ctx)}</li>`);
        i++;
        while (i < lines.length && /^\s{2,}\S/.test(lines[i]) && !/^\s*([-*+]|\d+[.)])\s+/.test(lines[i])) {
          items[items.length - 1] = items[items.length - 1].replace(/<\/li>$/, ` ${inline(lines[i++].trim(), ctx)}</li>`);
        }
      }
      out.push(`<${ordered ? 'ol' : 'ul'} data-line="${n}">${items.join('')}</${ordered ? 'ol' : 'ul'}>`);
      continue;
    }
    const para = [];
    while (i < lines.length && lines[i].trim() && !/^(#{1,6}\s|\s*(```|~~~)|\s*>|\s*([-*+]|\d+[.)])\s+)/.test(lines[i])) para.push(lines[i++]);
    if (!para.length) para.push(lines[i++]);
    out.push(`<p data-line="${n}">${inline(para.join('\n'), ctx)}</p>`);
  }
  return { html: out.join('\n'), headings, externalImages: ctx.external };
}
