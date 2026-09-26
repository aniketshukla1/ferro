// Tooltips, toasts and dialogs.
import { h } from '../core/dom.js';
import { keysEl } from '../core/keys.js';
import { icon } from './icons.js';

// ---------- tooltips ----------
// Any element with data-tip="Label" (and optional data-keys="Mod+B") gets a tooltip.
let tipEl = null;
let tipTimer = 0;
let tipTarget = null;
let warmUntil = 0;

function showTip(target) {
  const label = target.getAttribute('data-tip');
  if (!label) return;
  if (!tipEl) {
    tipEl = h('div', { class: 'tooltip', role: 'tooltip' });
    document.body.appendChild(tipEl);
  }
  tipEl.replaceChildren(h('span', null, label));
  const keys = target.getAttribute('data-keys');
  if (keys) tipEl.appendChild(keysEl(keys));
  tipEl.classList.remove('on');
  tipEl.style.left = '0px';
  tipEl.style.top = '0px';
  const r = target.getBoundingClientRect();
  const tr = tipEl.getBoundingClientRect();
  const side = target.getAttribute('data-tip-side') || (r.left < 60 ? 'right' : 'bottom');
  let x;
  let y;
  if (side === 'right') {
    x = r.right + 8;
    y = r.top + r.height / 2 - tr.height / 2;
  } else if (side === 'top') {
    x = r.left + r.width / 2 - tr.width / 2;
    y = r.top - tr.height - 8;
  } else {
    x = r.left + r.width / 2 - tr.width / 2;
    y = r.bottom + 8;
    if (y + tr.height > innerHeight - 4) y = r.top - tr.height - 8;
  }
  x = Math.max(6, Math.min(x, innerWidth - tr.width - 6));
  y = Math.max(6, y);
  tipEl.style.left = `${Math.round(x)}px`;
  tipEl.style.top = `${Math.round(y)}px`;
  requestAnimationFrame(() => tipEl?.classList.add('on'));
}

function hideTip() {
  clearTimeout(tipTimer);
  if (tipEl?.classList.contains('on')) warmUntil = Date.now() + 500;
  tipEl?.classList.remove('on');
  tipTarget = null;
}

export function installTooltips() {
  document.addEventListener('pointerover', (e) => {
    const t = e.target.closest?.('[data-tip]');
    if (!t || t === tipTarget) return;
    hideTip();
    tipTarget = t;
    tipTimer = setTimeout(() => showTip(t), Date.now() < warmUntil ? 60 : 480);
  });
  document.addEventListener('pointerout', (e) => {
    if (!tipTarget) return;
    const to = e.relatedTarget;
    if (to && tipTarget.contains(to)) return;
    hideTip();
  });
  document.addEventListener('pointerdown', hideTip, true);
  document.addEventListener('keydown', hideTip, true);
  document.addEventListener('scroll', hideTip, true);
}

// ---------- toasts ----------
let stack = null;
const TOAST_ICON = { info: 'info', ok: 'check-circle', warn: 'alert', error: 'alert' };

/**
 * @param {{kind?:'info'|'ok'|'warn'|'error', title:string, message?:string,
 *          action?:{label:string, run:()=>void}, timeout?:number}} t
 */
export function toast(t) {
  if (!stack) {
    stack = h('div', { class: 'toasts', 'aria-live': 'polite', role: 'status' });
    document.body.appendChild(stack);
  }
  const kind = t.kind || 'info';
  const titleEl = h('div', { class: 't-title' }, t.title);
  const msgEl = h('div', { class: 't-msg', hidden: !t.message }, t.message || '');
  const el = h('div', { class: `toast ${kind}` },
    h('span', { class: 't-icon' }, icon(TOAST_ICON[kind] || 'info')),
    h('div', { class: 't-body' },
      titleEl,
      msgEl,
      t.action ? h('div', { class: 't-actions' }, h('button', {
        class: 'btn sm',
        on: { click: () => { t.action.run(); close(); } },
      }, t.action.label)) : null),
    h('button', { class: 'icon-btn sm', 'aria-label': 'Dismiss', on: { click: () => close() } }, icon('x', 'sm')));
  stack.appendChild(el);
  let timer = 0;
  const arm = () => { if (t.timeout !== 0) timer = setTimeout(close, t.timeout || (kind === 'error' ? 8000 : 4000)); };
  el.addEventListener('mouseenter', () => clearTimeout(timer));
  el.addEventListener('mouseleave', arm);
  arm();
  function close() {
    clearTimeout(timer);
    if (!el.isConnected) return;
    el.classList.add('leaving');
    setTimeout(() => el.remove(), 160);
  }
  /** Replace the title and/or message of a live toast (progress steps). */
  function update({ title, message } = {}) {
    if (title != null) titleEl.textContent = title;
    if (message != null) {
      msgEl.textContent = message;
      msgEl.hidden = !message;
    }
  }
  return { close, update };
}

// ---------- dialogs ----------
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
