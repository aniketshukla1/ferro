// Image view (FRONTEND.md § 6.10): loads on first image open, not at boot.
import { h } from '../core/dom.js';
import { api } from '../core/api.js';
import { store } from '../core/store.js';
import { formatBytes } from '../core/util.js';
import { icon } from '../ui/icons.js';

const ZOOMS = [0.05, 0.1, 0.25, 0.5, 0.75, 1, 1.5, 2, 3, 4, 6, 8, 12, 16, 24, 32];

/**
 * Image view: fit / 1:1 / step zoom (buttons, +/-, Ctrl or ⌘ + wheel, pinch) around the pointer,
 * drag to pan, pixelated rendering past 200 %, checkerboard or plain background.
 */
export function createImageView(path, meta) {
  const img = h('img', { alt: path, src: api.rawUrl(path), draggable: 'false' });
  const stage = h('div', { class: 'imgv-stage', tabindex: '0', role: 'img', 'aria-label': path }, img);
  const zoomText = h('span', { class: 'num imgv-zoom' }, '—');
  const dims = h('span', { class: 'num' }, 'Loading…');
  const btn = (ic, label, keys, fn) => h('button', { class: 'icon-btn sm', 'aria-label': label, 'data-tip': label, 'data-keys': keys, on: { click: fn } }, icon(ic, 'sm'));
  const fitBtn = h('button', { class: 'btn ghost sm', 'aria-pressed': 'true', 'data-tip': 'Fit to window', 'data-keys': '0', on: { click: () => fit() } }, 'Fit');
  const oneBtn = h('button', { class: 'btn ghost sm', 'aria-pressed': 'false', 'data-tip': 'Actual size', 'data-keys': '1', on: { click: () => setZoom(1) } }, '1:1');
  const bgBtn = btn('contrast', 'Toggle background', null, () => stage.classList.toggle('plain'));
  const bar = h('div', { class: 'imgv-bar' },
    dims, h('span', { class: 'imgv-sp' }),
    btn('minus', 'Zoom out', '-', () => step(-1)), zoomText, btn('plus', 'Zoom in', '+', () => step(1)),
    h('span', { class: 'fb-sep' }), fitBtn, oneBtn, bgBtn);
  const el = h('div', { class: 'imgv' }, stage, bar);
  let zoom = 1;
  let fitMode = true;

  function fitScale() {
    if (!img.naturalWidth) return 1;
    const pad = 48;
    return Math.min(1, (stage.clientWidth - pad) / img.naturalWidth, (stage.clientHeight - pad) / img.naturalHeight) || 1;
  }
  function apply(anchor) {
    const before = { w: img.width || 1, sl: stage.scrollLeft, st: stage.scrollTop };
    img.style.width = `${Math.max(1, Math.round(img.naturalWidth * zoom))}px`;
    img.style.height = `${Math.max(1, Math.round(img.naturalHeight * zoom))}px`;
    img.classList.toggle('pixelated', zoom >= 2);
    zoomText.textContent = `${Math.round(zoom * 100)}%`;
    fitBtn.setAttribute('aria-pressed', String(fitMode));
    oneBtn.setAttribute('aria-pressed', String(!fitMode && zoom === 1));
    if (anchor) {
      // keep the point under the cursor fixed while zooming
      const k = (img.naturalWidth * zoom) / before.w;
      stage.scrollLeft = (before.sl + anchor.x) * k - anchor.x;
      stage.scrollTop = (before.st + anchor.y) * k - anchor.y;
    }
  }
  function fit() { fitMode = true; zoom = fitScale(); apply(); }
  function setZoom(z, anchor) { fitMode = false; zoom = Math.min(32, Math.max(0.05, z)); apply(anchor); }
  function step(dir, anchor) {
    const next = dir > 0 ? ZOOMS.find((z) => z > zoom + 1e-6) : [...ZOOMS].reverse().find((z) => z < zoom - 1e-6);
    if (next) setZoom(next, anchor);
  }

  img.addEventListener('load', () => {
    dims.textContent = `${img.naturalWidth} × ${img.naturalHeight} · ${formatBytes(meta.size)}`;
    fit();
  });
  img.addEventListener('error', () => { dims.textContent = 'Cannot display this image'; });
  stage.addEventListener('wheel', (e) => {
    if (!e.ctrlKey && !e.metaKey) return; // plain wheel scrolls; pinch arrives as ctrl+wheel
    e.preventDefault();
    const r = stage.getBoundingClientRect();
    setZoom(zoom * Math.exp(-e.deltaY * 0.01), { x: e.clientX - r.left, y: e.clientY - r.top });
  }, { passive: false });
  stage.addEventListener('pointerdown', (e) => {
    if (e.button !== 0 || (stage.scrollWidth <= stage.clientWidth && stage.scrollHeight <= stage.clientHeight)) return;
    const x0 = e.clientX; const y0 = e.clientY; const sl = stage.scrollLeft; const st = stage.scrollTop;
    stage.setPointerCapture(e.pointerId);
    stage.classList.add('panning');
    const move = (ev) => { stage.scrollLeft = sl - (ev.clientX - x0); stage.scrollTop = st - (ev.clientY - y0); };
    const up = () => { stage.classList.remove('panning'); stage.removeEventListener('pointermove', move); stage.removeEventListener('pointerup', up); };
    stage.addEventListener('pointermove', move);
    stage.addEventListener('pointerup', up);
  });
  stage.addEventListener('keydown', (e) => {
    if (e.metaKey || e.ctrlKey || e.altKey) return;
    if (e.key === '+' || e.key === '=') { e.preventDefault(); step(1); }
    else if (e.key === '-') { e.preventDefault(); step(-1); }
    else if (e.key === '0') { e.preventDefault(); fit(); }
    else if (e.key === '1') { e.preventDefault(); setZoom(1); }
  });
  const ro = new ResizeObserver(() => { if (fitMode && img.naturalWidth) fit(); });
  ro.observe(stage);

  return {
    el,
    kind: 'image',
    focus() { stage.focus({ preventScroll: true }); },
    state: () => (fitMode ? null : { zoom }),
    restore(s) { if (s?.zoom) { fitMode = false; zoom = s.zoom; } },
    onShow() { store.set('cursor', { path, image: true, language: meta.language || 'Image' }); if (fitMode && img.naturalWidth) fit(); },
    destroy() { ro.disconnect(); },
  };
}
