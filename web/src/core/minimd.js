// Escape-first mini markdown renderer for streaming AI answers (FRONTEND.md § 4 rule 7).
// parseMiniMarkdown never interprets '<', '>', or any markup: anything that is not one of a
// few plain inline marks (`code`, **bold**, *em*) is emitted as a literal text run. The DOM
// renderer below turns those runs into text nodes and h() elements only, so raw HTML or script
// payloads arriving mid-stream can never execute — they render as visible text until the turn
// finishes and the final answer is swapped for the backend-sanitized markdown (rule 7).
import { h } from './dom.js';

const INLINE = [
  { type: 'code', re: /`([^`\n]+)`/ },
  { type: 'bold', re: /\*\*([^*\n]+)\*\*/ },
  { type: 'em', re: /\*([^*\n]+)\*/ },
];

/** Tokenize into {type:'text'|'code'|'bold'|'em'|'br', value}[]. Pure — no DOM, no HTML parsing. */
export function parseMiniMarkdown(text) {
  const tokens = [];
  const lines = String(text ?? '').split('\n');
  lines.forEach((line, li) => {
    let rest = line;
    while (rest) {
      let best = null;
      for (const rule of INLINE) {
        const m = rule.re.exec(rest);
        if (m && (!best || m.index < best.m.index)) best = { m, rule };
      }
      if (!best) { tokens.push({ type: 'text', value: rest }); break; }
      const { m, rule } = best;
      if (m.index > 0) tokens.push({ type: 'text', value: rest.slice(0, m.index) });
      tokens.push({ type: rule.type, value: m[1] });
      rest = rest.slice(m.index + m[0].length);
    }
    if (li < lines.length - 1) tokens.push({ type: 'br' });
  });
  return tokens;
}

const TAG = { code: 'code', bold: 'strong', em: 'em' };

/** Render `text` into `container` using h()/textContent only — never innerHTML. */
export function renderMiniMarkdown(container, text) {
  container.replaceChildren();
  for (const tok of parseMiniMarkdown(text)) {
    if (tok.type === 'br') container.appendChild(document.createElement('br'));
    else if (tok.type === 'text') container.appendChild(document.createTextNode(tok.value));
    else container.appendChild(h(TAG[tok.type], null, tok.value));
  }
}

/**
 * Tracks a streaming answer's render mode so the final backend-rendered markdown replaces the
 * mini-rendered text exactly once. Pure state — no DOM — so it's unit-testable directly.
 */
export function createAnswerStream() {
  let finalized = false;
  let buffer = '';
  return {
    get finalized() { return finalized; },
    get text() { return buffer; },
    /** Append a stream token. Returns the accumulated text, or null once finalized (tokens after `final` are ignored). */
    token(text) {
      if (finalized) return null;
      buffer += text;
      return buffer;
    },
    /** Swap to the final backend-rendered markdown. Returns the html to render, or false if already finalized (idempotent). */
    final(html) {
      if (finalized) return false;
      finalized = true;
      return html;
    },
  };
}
