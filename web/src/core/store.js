// Tiny reactive store with per-slice subscriptions. Notifications are batched in a
// microtask so several updates in one tick trigger one render per subscriber.

export function createStore(initial) {
  let state = { ...initial };
  const subs = new Map();
  const pending = new Set();
  let scheduled = false;

  function flush() {
    scheduled = false;
    const keys = [...pending];
    pending.clear();
    for (const key of keys) {
      for (const fn of subs.get(key) || []) call(fn, state[key], key);
      for (const fn of subs.get('*') || []) call(fn, state[key], key);
    }
  }
  function call(fn, value, key) {
    try { fn(value, key); } catch (e) { console.error('[store]', key, e); }
  }
  function notify(key) {
    pending.add(key);
    if (!scheduled) {
      scheduled = true;
      queueMicrotask(flush);
    }
  }

  return {
    get: (key) => state[key],
    set(key, value) {
      if (Object.is(state[key], value)) return;
      state[key] = value;
      notify(key);
    },
    update(key, patch) {
      state[key] = { ...(state[key] || {}), ...patch };
      notify(key);
    },
    /** Subscribe to a slice ('*' = every change). Returns an unsubscribe function. */
    subscribe(key, fn, { now = false } = {}) {
      if (!subs.has(key)) subs.set(key, new Set());
      subs.get(key).add(fn);
      if (now) call(fn, state[key], key);
      return () => subs.get(key)?.delete(fn);
    },
    /** Replace every slice (workspace switch). */
    reset(next) {
      const keys = new Set([...Object.keys(state), ...Object.keys(next)]);
      state = { ...next };
      keys.forEach(notify);
    },
  };
}

export const initialState = () => ({
  meta: null,
  conn: 'connecting', // connecting | connected | reconnecting | unauthorized | offline
  index: { state: 'indexing', files: 0, ms: 0, generation: 0, searchIndex: 'off' },
  settings: {},
  git: null,
  pr: null,
  metrics: null,
  timing: null, // { label, serverMs, clientMs, at }
  jobs: new Map(),
  cursor: null, // { path, line, col, selLines }
  active: null, // active document path
});

export const store = createStore(initialState());
