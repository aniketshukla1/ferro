// UI event bus for cross-feature signals (open file, reveal, focus, …).

const listeners = new Map();

export const bus = {
  on(type, fn) {
    if (!listeners.has(type)) listeners.set(type, new Set());
    listeners.get(type).add(fn);
    return () => listeners.get(type)?.delete(fn);
  },
  emit(type, payload) {
    for (const fn of listeners.get(type) || []) {
      try { fn(payload); } catch (e) { console.error('[bus]', type, e); }
    }
  },
};
