// Theme registry, application, light/dark toggle and the live-preview picker (palette mode).
// Changes dispatch a `ferro:theme` event on document so chrome (e.g. the toggle icon) can follow.
import { api } from '../core/api.js';
import { storage } from '../core/util.js';

export const THEMES = [
  { id: 'graphite', name: 'Graphite', type: 'dark', note: 'default dark' },
  { id: 'porcelain', name: 'Porcelain', type: 'light', note: 'default light' },
  { id: 'carbon', name: 'Carbon', type: 'dark', note: 'true black' },
];

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

function apply(resolved) {
  if (document.documentElement.dataset.theme === resolved) return;
  document.documentElement.dataset.theme = resolved;
  document.dispatchEvent(new CustomEvent('ferro:theme', { detail: { theme: resolved } }));
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

/** Flip between the default light and dark themes (keeps carbon when going back to dark from it). */
export function toggleLightDark() {
  const next = isDarkTheme() ? LIGHT : (storage.get('ferro.lastDark', DARK));
  if (isDarkTheme()) storage.set('ferro.lastDark', resolveTheme(current));
  setTheme(next);
  return THEMES.find((t) => t.id === next);
}

/** Next theme in the list. */
export function cycleTheme() {
  const resolved = resolveTheme(current);
  const i = THEMES.findIndex((t) => t.id === resolved);
  const next = THEMES[(i + 1) % THEMES.length];
  setTheme(next.id);
  return next;
}
