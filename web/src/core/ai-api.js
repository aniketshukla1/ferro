// AI + commit-message API wrappers (F4, API.md § 10). Kept out of core/api.js, which is on the
// boot graph (§ 9): only the on-demand features/ai.js and features/git.js call these, so the SSE
// stream reader below never has to be paid for by every session.
import { request, isAbort, ApiError, HTTP_CODES, apiUrl, getTransport } from './api.js';

/** POST `body`, read the response as a `text/event-stream` (fetch, not EventSource — it cannot POST). */
async function realStream(path, body, signal, onEvent) {
  const res = await fetch(apiUrl(path), {
    method: 'POST',
    signal,
    credentials: 'same-origin',
    headers: { 'content-type': 'application/json', accept: 'text/event-stream' },
    body: JSON.stringify(body),
  });
  if (!res.ok) {
    let data = null;
    try { data = await res.json(); } catch { /* not JSON */ }
    const err = data?.error || {};
    throw new ApiError(res.status, err.code || HTTP_CODES[res.status] || 'internal', err.message || res.statusText, err.detail);
  }
  const reader = res.body.getReader();
  const decoder = new TextDecoder();
  let buf = '';
  let event = 'message';
  try {
    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      buf += decoder.decode(value, { stream: true });
      let idx;
      while ((idx = buf.indexOf('\n\n')) >= 0) {
        const raw = buf.slice(0, idx);
        buf = buf.slice(idx + 2);
        let data = '';
        for (const line of raw.split('\n')) {
          if (line.startsWith('event:')) event = line.slice(6).trim();
          else if (line.startsWith('data:')) data += (data ? '\n' : '') + line.slice(5).trim();
        }
        if (data) { let parsed = null; try { parsed = JSON.parse(data); } catch { /* ignore malformed */ } onEvent(event, parsed); }
        event = 'message';
      }
    }
  } finally {
    reader.cancel().catch(() => {});
  }
}

/** Mock mode installs `transport.stream`; the real transport streams over fetch (above). */
async function streamRequest(path, body, opts) {
  try {
    const transport = getTransport();
    if (transport.stream) await transport.stream(path, body, opts);
    else await realStream(path, body, opts.signal, opts.onEvent);
  } catch (e) {
    if (isAbort(e)) return;
    if (e instanceof ApiError) throw e;
    throw new ApiError(0, 'network', 'Cannot reach the ferro server', { cause: String(e) });
  }
}

export const aiApi = {
  renderMarkdown: (text, opts) => request('markdown/render', { method: 'POST', body: { text, ...opts } }),
  status: () => request('ai/status'),
  /** Streamed: POST /ai/ask. `opts.onEvent(event, data)` receives meta/token/tool_start/tool_result/final/usage/error. */
  ask: (body, opts) => streamRequest('ai/ask', body, opts),
  /** Streamed: POST /ai/edit (§ 10.9) — meta/token/final/usage/error; nothing is written. */
  edit: (body, opts) => streamRequest('ai/edit', body, opts),
  review: (body) => request('ai/review', { method: 'POST', body }),
  acceptFinding: (id, body = {}) => request(`ai/findings/${encodeURIComponent(id)}/accept`, { method: 'POST', body }),
  dismissFinding: (id, body = {}) => request(`ai/findings/${encodeURIComponent(id)}/dismiss`, { method: 'POST', body }),
  commitMessage: () => request('git/commit-message', { method: 'POST', body: {} }),
};
