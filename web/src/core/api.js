// API v1 client (docs/spec/API.md). One transport: HTTP + SSE.
// Mock mode swaps the transport for src/mock (no backend needed).
import { store } from './store.js';

export class ApiError extends Error {
  constructor(status, code, message, detail) {
    super(message || code);
    this.name = 'ApiError';
    this.status = status;
    this.code = code;
    this.detail = detail;
  }
}

const HTTP_CODES = { 400: 'bad_request', 401: 'unauthorized', 403: 'forbidden', 404: 'not_found', 409: 'conflict', 413: 'too_large', 422: 'unsupported', 429: 'rate_limited', 502: 'upstream', 503: 'not_ready' };

function apiUrl(path, query) {
  const url = new URL(`api/v1/${path}`, document.baseURI);
  if (query) {
    for (const k in query) {
      const v = query[k];
      if (v === undefined || v === null || v === '') continue;
      url.searchParams.set(k, typeof v === 'boolean' ? (v ? '1' : '0') : String(v));
    }
  }
  return url;
}

function parseServerTiming(h) {
  if (!h) return null;
  const m = /dur=([\d.]+)/.exec(h);
  return m ? parseFloat(m[1]) : null;
}

const http = {
  async request(path, { method = 'GET', query, body, signal } = {}) {
    const url = apiUrl(path, query);
    const t0 = performance.now();
    let res;
    try {
      res = await fetch(url, {
        method,
        signal,
        credentials: 'same-origin',
        headers: body !== undefined ? { 'content-type': 'application/json' } : undefined,
        body: body !== undefined ? JSON.stringify(body) : undefined,
      });
    } catch (e) {
      if (e?.name === 'AbortError') throw e;
      throw new ApiError(0, 'network', 'Cannot reach the ferro server', { cause: String(e) });
    }
    const clientMs = performance.now() - t0;
    const serverMs = parseServerTiming(res.headers.get('server-timing'));
    if (res.status === 204) return null;
    const ct = res.headers.get('content-type') || '';
    const data = ct.includes('json') ? await res.json().catch(() => null) : await res.text();
    if (!res.ok) {
      const err = (data && data.error) || {};
      throw new ApiError(res.status, err.code || HTTP_CODES[res.status] || 'internal', err.message || res.statusText, err.detail);
    }
    return { data, clientMs, serverMs };
  },
  eventsUrl(query) {
    return apiUrl('events', query).toString();
  },
};

let transport = http;

/** Install a transport ({ request, events? }). Used by mock mode. */
export function setTransport(t) {
  transport = t;
}
export const getTransport = () => transport;

/**
 * Low-level request. Returns parsed JSON. Records timing for the status bar.
 * @param {string} path  path under /api/v1/
 * @param {{method?:string, query?:object, body?:any, signal?:AbortSignal, label?:string}} [opts]
 */
export async function request(path, opts = {}) {
  try {
    const { data, clientMs, serverMs } = await transport.request(path, opts);
    if (opts.label) store.set('timing', { label: opts.label, clientMs, serverMs, at: Date.now() });
    return data;
  } catch (e) {
    if (e instanceof ApiError) {
      if (e.status === 401) store.set('conn', 'unauthorized');
      else if (e.code === 'network') store.set('conn', 'offline');
    }
    throw e;
  }
}

export const isAbort = (e) => e?.name === 'AbortError';

/** Typed wrappers for the v1 surface used by the frontend. */
export const api = {
  meta: (o) => request('meta', o),
  settings: (scope = 'effective', o) => request('settings', { query: { scope }, ...o }),
  putSettings: (values, scope = 'user') => request('settings', { method: 'PUT', query: { scope }, body: { values } }),
  settingsSchema: () => request('settings/schema'),
  session: () => request('session'),
  putSession: (data) => request('session', { method: 'PUT', body: { data } }),
  tree: (dir = '', o) => request('tree', { query: { dir }, ...o }),
  file: (path, o) => request('file', { query: { path }, ...o }),
  lines: (path, from, count, o) => request('file/lines', { query: { path, from, count, hl: 1 }, ...o }),
  outline: (path, o) => request('file/outline', { query: { path }, ...o }),
  markdown: (path, o) => request('file/markdown', { query: { path }, ...o }),
  find: (path, q, opts, o) => request('file/find', { query: { path, q, ...opts }, ...o }),
  fuzzy: (q, limit = 50, boost, o) => request('fuzzy', { query: { q, limit, boost: boost?.length ? boost.join(',') : undefined }, label: 'fuzzy', ...o }),
  search: (params, o) => request('search', { query: params, label: 'search', ...o }),
  resolve: (candidates) => request('paths/resolve', { method: 'POST', body: { candidates } }),
  gitStatus: (o) => request('git/status', o),
  metrics: () => request('metrics'),
  jobs: () => request('jobs'),
  rebuildIndex: () => request('index/rebuild', { method: 'POST', body: {} }),
  openExternal: (url) => request('desktop/open-external', { method: 'POST', body: { url } }),
  rawUrl: (path) => (transport.rawUrl ? transport.rawUrl(path) : apiUrl('file/raw', { path }).toString()),
};

/** Feature-flag check against meta.features. */
export function has(feature) {
  const f = store.get('meta')?.features;
  return Array.isArray(f) && f.includes(feature);
}
