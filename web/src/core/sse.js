// Events client for GET /api/v1/events (docs/spec/API.md § 12).
// Updates the store for state-carrying events and re-emits everything on the bus as `ev:<type>`.
import { store } from './store.js';
import { bus } from './bus.js';
import { getTransport } from './api.js';

const TYPES = ['hello', 'index', 'fs', 'git', 'hl', 'settings', 'workspace', 'job', 'pr', 'threads', 'drafts', 'metrics', 'resync'];

let source = null;
let downSince = 0;
let backoff = 1000;
let retryTimer = 0;
let graceTimer = 0;

function dispatch(type, data) {
  switch (type) {
    case 'index': store.set('index', data); break;
    case 'git': store.set('git', data); break;
    case 'metrics': store.set('metrics', data); break;
    case 'pr': store.set('pr', data); break;
    case 'settings': store.set('settings', data?.values || {}); break;
    case 'job': {
      const jobs = new Map(store.get('jobs'));
      jobs.set(data.id, data);
      store.set('jobs', jobs);
      break;
    }
    default: break;
  }
  bus.emit(`ev:${type}`, data);
}

function onOpen() {
  clearTimeout(graceTimer);
  downSince = 0;
  backoff = 1000;
  if (store.get('conn') !== 'unauthorized') store.set('conn', 'connected');
}

function onDown() {
  if (!downSince) downSince = Date.now();
  // Short blips don't deserve a banner; after 3 s show "reconnecting".
  clearTimeout(graceTimer);
  graceTimer = setTimeout(() => {
    if (downSince && store.get('conn') === 'connected') store.set('conn', 'reconnecting');
  }, 3000);
}

/** Connect (or reconnect) the events stream. */
export function connectEvents({ metrics = true } = {}) {
  disconnectEvents();
  const t = getTransport();
  if (t.events) {
    // mock transport provides its own stream
    source = t.events({ onEvent: dispatch, onOpen, onError: onDown, metrics });
    return;
  }
  // API.md says ?metrics=1; early B1 builds only parsed a strict bool, so send "true" (accepted by all).
  const es = new EventSource(t.eventsUrl({ metrics: metrics ? 'true' : undefined }));
  source = es;
  es.onopen = onOpen;
  es.onerror = () => {
    onDown();
    if (es.readyState === EventSource.CLOSED) {
      // Browser gave up (e.g. 401 or server gone): retry with backoff.
      clearTimeout(retryTimer);
      retryTimer = setTimeout(() => connectEvents({ metrics }), backoff);
      backoff = Math.min(backoff * 2, 8000);
    }
  };
  for (const type of TYPES) {
    es.addEventListener(type, (ev) => {
      let data = null;
      try { data = ev.data ? JSON.parse(ev.data) : null; } catch { /* ignore malformed */ }
      dispatch(type, data);
    });
  }
}

export function disconnectEvents() {
  clearTimeout(retryTimer);
  if (source) {
    source.close?.();
    source = null;
  }
}
