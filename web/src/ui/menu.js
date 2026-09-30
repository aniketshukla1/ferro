// Popup context menu (loaded on the first right-click; not part of the boot graph).
import { h } from '../core/dom.js';
import { keysEl } from '../core/keys.js';
import { icon } from './icons.js';

/**
 * @param {{x:number, y:number, items:Array<{label?:string, icon?:string, keys?:string, disabled?:boolean, run?:()=>void, sep?:boolean}>, onClose?:()=>void}} o
 */
export function openMenu(o) {
  const menu = h('div', { class: 'menu', role: 'menu' });
  for (const it of o.items) {
    if (it.sep) {
      menu.appendChild(h('div', { class: 'menu-sep' }));
      continue;
    }
    const btn = h('button', {
      class: 'menu-item',
      role: 'menuitem',
      disabled: !!it.disabled,
      on: { click: () => { close(); it.run?.(); } },
    }, it.icon ? icon(it.icon, 'sm') : null, h('span', null, it.label));
    if (it.keys) btn.appendChild(keysEl(it.keys));
    menu.appendChild(btn);
  }
  menu.style.left = `${Math.max(0, Math.min(o.x, innerWidth - 220))}px`;
  menu.style.top = `${Math.max(0, Math.min(o.y, innerHeight - (o.items.length * 30 + 20)))}px`;
  document.body.appendChild(menu);

  // Registered right away (the right-click that opened the menu is already over): a deferred
  // registration outlived a menu closed within the delay and swallowed every later Escape.
  function onDocClick(e) { if (!menu.contains(e.target)) close(); }
  function onDocKey(e) { if (e.key === 'Escape') { e.preventDefault(); close(); } }
  document.addEventListener('pointerdown', onDocClick, { capture: true });
  document.addEventListener('keydown', onDocKey, { capture: true });

  let closed = false;
  function close() {
    if (closed) return;
    closed = true;
    document.removeEventListener('pointerdown', onDocClick, true);
    document.removeEventListener('keydown', onDocKey, true);
    menu.remove();
    o.onClose?.();
  }
  return { close, el: menu };
}
