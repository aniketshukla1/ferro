// Presentation preferences from ui.* settings, applied at boot and on every settings change.
// (The settings dialog itself lives in settings.js and loads on first use.)

/** Apply ui.* values that change presentation: code font size and file-icon tint. */
export function applyUiSettings(values = {}) {
  const root = document.documentElement;
  const fs = Number(values['ui.codeFontSize']);
  if (fs >= 10 && fs <= 20) {
    root.style.setProperty('--code-fs', `${fs}px`);
    root.style.setProperty('--code-lh', `${Math.round(fs * 1.54)}px`);
  } else {
    root.style.removeProperty('--code-fs');
    root.style.removeProperty('--code-lh');
  }
  root.classList.toggle('icons-color', values['ui.fileIconColors'] === true);
}
