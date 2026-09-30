// Presentation preferences from ui.* settings, applied at boot and on every settings change.
// (The settings dialog itself lives in settings.js and loads on first use.)

/** Apply ui.* values that change presentation: code font size and line height, icon tint, ruler. */
export function applyUiSettings(values = {}) {
  const root = document.documentElement;
  const n = Number(values['ui.codeFontSize']);
  const fs = n >= 10 && n <= 20 ? n : null;
  const p = Number(values['ui.codeLineHeight']);
  const lh = p >= 120 && p <= 200 ? p / 100 : null;
  if (fs) root.style.setProperty('--code-fs', `${fs}px`);
  else root.style.removeProperty('--code-fs');
  if (fs || lh) root.style.setProperty('--code-lh', `${Math.round((fs || 13) * (lh || 1.54))}px`);
  else root.style.removeProperty('--code-lh');
  root.classList.toggle('icons-color', values['ui.fileIconColors'] === true);
  root.classList.toggle('no-ruler', values['ui.overviewRuler'] === false);
}
