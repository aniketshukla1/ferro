// Connection banner: shown while the events stream is down, with a toast when it recovers.
import { h } from '../core/dom.js';
import { store } from '../core/store.js';
import { toast } from '../ui/overlay.js';

export function watchConnection(bannerHost) {
  let banner = null;
  let wasDown = false;
  store.subscribe('conn', (c) => {
    const down = c === 'reconnecting' || c === 'offline';
    if (down && !banner) {
      banner = h('div', { class: 'banner', role: 'status' }, h('span', { class: 'spinner' }), h('span', null, 'Lost connection to the ferro server — reconnecting…'));
      bannerHost.appendChild(banner);
    } else if (!down && banner) {
      banner.remove();
      banner = null;
      if (wasDown && c === 'connected') toast({ kind: 'ok', title: 'Reconnected', timeout: 2000 });
    }
    wasDown = down;
  });
}
