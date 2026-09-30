// Theme registry, application, light/dark toggle and live preview. Changes dispatch `ferro:theme`
// on document. graphite / porcelain / carbon are boot CSS; pack themes load on first use.
import { api } from '../core/api.js';
import { storage } from '../core/util.js';

const list = (type, pack, ids) => ids.split(' ').map((id) => ({ id, name: id[0].toUpperCase() + id.slice(1), type, pack }));
export const THEMES = [
  ...list('dark', false, 'graphite'), ...list('light', false, 'porcelain'), ...list('dark', false, 'carbon'),
  ...list('dark', true, 'slate fjord abyss moss umber dusk'),
  ...list('light', true, 'paper mist sand sage frost dawn chalk'),
];

let pack = null;
/** Load themes-extra.css once (boot-theme.js may have linked it). */
export function loadThemePack() {
  let l = document.getElementById('theme-pack');
  if (!l) document.head.append(l = Object.assign(document.createElement('link'), { id: 'theme-pack', rel: 'stylesheet', href: 'styles/themes-extra.css' }));
  return (pack ||= l.sheet ? Promise.resolve() : new Promise((done) => { l.onload = l.onerror = done; }));
}

const DARK = 'graphite';
const LIGHT = 'porcelain';
const KEY = 'ferro.theme';
const media = window.matchMedia?.('(prefers-color-scheme: light)');

/** 'auto' resolves to porcelain/graphite from the OS preference; unknown ids fall back the same way. */
export function resolveTheme(id) {
  if (id && id !== 'auto' && THEMES.some((t) => t.id === id)) return id;
  return media?.matches ? LIGHT : DARK;
}

let current = storage.get(KEY, 'auto');

export function currentTheme() { return current; }
export const isDarkTheme = (id = current) => THEMES.find((t) => t.id === resolveTheme(id))?.type !== 'light';

let wanted = null;
function apply(id) {
  wanted = id;
  // pack themes wait for their CSS (no flash of the stand-in)
  if (THEMES.find((t) => t.id === id)?.pack) loadThemePack().then(() => wanted === id && commit(id));
  else commit(id);
}

function commit(id) {
  if (document.documentElement.dataset.theme === id) return;
  document.documentElement.dataset.theme = id;
  document.dispatchEvent(new CustomEvent('ferro:theme', { detail: { theme: id } }));
}

/** Apply without persisting (used for live preview). */
export function previewTheme(id) {
  apply(resolveTheme(id));
}

/** Apply and persist (localStorage for flash-free boot, settings for sync). */
export function setTheme(id, { persist = true } = {}) {
  current = id || 'auto';
  previewTheme(current);
  storage.set(KEY, current);
  if (persist) api.putSettings({ 'ui.theme': current }).catch(() => {});
}

export function applySettingsTheme(values) {
  const id = values?.['ui.theme'];
  if (id && id !== current) {
    current = THEMES.some((t) => t.id === id) || id === 'auto' ? id : 'auto';
    storage.set(KEY, current);
  }
  previewTheme(current);
}

media?.addEventListener?.('change', () => { if (current === 'auto') previewTheme('auto'); });

/** Flip light / dark, back to the last theme used on that side (porcelain / graphite at first). */
export function toggleLightDark() {
  const dark = isDarkTheme();
  storage.set(`ferro.last${dark ? 'Dark' : 'Light'}`, resolveTheme(current));
  const saved = storage.get(`ferro.last${dark ? 'Light' : 'Dark'}`, null);
  const next = THEMES.find((t) => t.id === saved && isDarkTheme(t.id) !== dark) || THEMES.find((t) => t.id === (dark ? LIGHT : DARK));
  setTheme(next.id);
  return next;
}

/** Next theme in the list. */
export function cycleTheme() {
  const resolved = resolveTheme(current);
  const i = THEMES.findIndex((t) => t.id === resolved);
  const next = THEMES[(i + 1) % THEMES.length];
  setTheme(next.id);
  return next;
}
