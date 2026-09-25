// Frontend-owned session blob, persisted per workspace through PUT /api/v1/session.
// Holds tabs, layout, open dirs and recents so a reload (or a restart) restores the workbench.
import { api } from '../core/api.js';
import { debounce } from '../core/util.js';

const VERSION = 1;

const defaults = () => ({
  v: VERSION,
  tabs: [], // [{ path, preview, line, scrollTop }]
  active: null,
  openDirs: [],
  recent: [], // most recent first
  layout: { sidebar: true, sidebarW: 272, inspector: false, inspectorW: 340, panel: 'files' },
});

let data = defaults();

export const session = {
  get data() { return data; },
  load(raw) {
    data = defaults();
    if (raw && typeof raw === 'object' && raw.v === VERSION) {
      data = { ...data, ...raw, layout: { ...data.layout, ...(raw.layout || {}) } };
    }
    return data;
  },
  update(patch) {
    Object.assign(data, patch);
    save();
  },
  layout(patch) {
    data.layout = { ...data.layout, ...patch };
    save();
  },
  touchRecent(path) {
    data.recent = [path, ...data.recent.filter((p) => p !== path)].slice(0, 30);
    save();
  },
  saveNow() { save.cancel(); persist(); },
};

async function persist() {
  try { await api.putSession(data); } catch { /* offline: next save retries */ }
}
const save = debounce(persist, 800);
window.addEventListener('pagehide', () => { save.cancel(); persist(); });
