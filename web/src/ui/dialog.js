// Modal dialogs. Only on-demand features open dialogs, so this stays out of the boot graph.
import { h } from '../core/dom.js';
import { icon } from './icons.js';

/**
 * Modal dialog with focus trap. Returns { close, el }.
 * @param {{title:string, body:Node, actions?:Array<{label:string, primary?:boolean, run?:()=>any}>,
 *          width?:string, onClose?:()=>void, className?:string}} o
 */
export function openDialog(o) {
  const prevFocus = document.activeElement;
  const dialog = h('div', {
    class: `dialog ${o.className || ''}`,
    role: 'dialog',
    'aria-modal': 'true',
    'aria-label': o.title,
  },
  h('div', { class: 'dialog-head' },
    h('h2', null, o.title),
    h('button', { class: 'icon-btn', 'aria-label': 'Close', on: { click: () => close() } }, icon('x'))),
  h('div', { class: 'dialog-body' }, o.body),
  o.actions?.length ? h('div', { class: 'dialog-foot' }, o.actions.map((a) => h('button', {
    class: a.primary ? 'btn primary' : 'btn',
    on: { click: async () => { const r = await a.run?.(); if (r !== false) close(); } },
  }, a.label))) : null);
  if (o.width) dialog.style.width = o.width;
  const overlay = h('div', { class: 'overlay', on: { mousedown: (e) => { if (e.target === overlay) close(); } } }, dialog);
  document.body.appendChild(overlay);

  function onKey(e) {
    if (e.key === 'Escape') {
      e.preventDefault();
      e.stopPropagation();
      close();
    } else if (e.key === 'Tab') {
      const f = [...dialog.querySelectorAll('button, [href], input, select, textarea, [tabindex]:not([tabindex="-1"])')].filter((x) => !x.disabled && x.offsetParent !== null);
      if (!f.length) return;
      const first = f[0];
      const last = f[f.length - 1];
      if (e.shiftKey && document.activeElement === first) { e.preventDefault(); last.focus(); }
      else if (!e.shiftKey && document.activeElement === last) { e.preventDefault(); first.focus(); }
    }
  }
  // Listen on the document: focus can leave the dialog (e.g. a clicked node is re-rendered)
  // and Escape must still close the top-most dialog.
  const docKey = (e) => {
    const top = [...document.querySelectorAll('.overlay')].pop();
    if (top === overlay) onKey(e);
  };
  document.addEventListener('keydown', docKey, true);
  // Focus synchronously: keys typed right after the opening shortcut must land in the dialog.
  const auto = dialog.querySelector('[autofocus], input, textarea, .btn.primary') || dialog.querySelector('button');
  auto?.focus();
  let closed = false;
  function close() {
    if (closed) return;
    closed = true;
    document.removeEventListener('keydown', docKey, true);
    overlay.remove();
    o.onClose?.();
    if (prevFocus && prevFocus.isConnected) prevFocus.focus?.();
  }
  return { close, el: dialog };
}
