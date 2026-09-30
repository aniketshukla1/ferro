// Latency HUD (FRONTEND.md § 9; ui.hud, Mod+Alt+P): frame times, long tasks, per-request client
// and server times, heap. Loads on first use; measures nothing while hidden.
import { h, mount } from '../core/dom.js';
import { timingHooks } from '../core/api.js';
import { formatMs } from '../core/util.js';
import { icon } from '../ui/icons.js';

let hud = null;

export function isOpen() { return !!hud; }

/** Show or hide the HUD; returns whether it is now open. `onClose` runs when its × is used. */
export function toggleHud(on = !hud, { onClose } = {}) {
  if (on && !hud) hud = createHud(onClose);
  else if (!on && hud) { hud.destroy(); hud = null; }
  return !!hud;
}

function createHud(onClose) {
  const frameEl = h('span', { class: 'num' }, '—');
  const longEl = h('span', { class: 'num' }, '—');
  const heapEl = h('span', { class: 'num' }, '—');
  const bootEl = h('span', { class: 'num' }, bootMarks());
  const reqList = h('ol', { class: 'hud-reqs' });
  const el = h('aside', { class: 'hud', 'aria-label': 'Latency HUD' },
    h('div', { class: 'hud-head' }, h('span', { class: 'hud-title' }, 'Latency'),
      h('button', { class: 'icon-btn xs', 'aria-label': 'Close latency HUD', on: { click: () => { toggleHud(false); onClose?.(); } } }, icon('x', 'xs'))),
    h('dl', { class: 'hud-stats' },
      h('dt', null, 'Frames'), h('dd', null, frameEl),
      h('dt', null, 'Long tasks'), h('dd', null, longEl),
      h('dt', null, 'Heap'), h('dd', null, heapEl),
      h('dt', null, 'Boot'), h('dd', null, bootEl)),
    reqList);
  document.body.appendChild(el);

  // Frames: p50/p95 interval over the last 120 animation frames.
  const frames = [];
  let last = performance.now();
  let raf = requestAnimationFrame(function tick(t) {
    frames.push(t - last);
    last = t;
    if (frames.length > 120) frames.shift();
    raf = requestAnimationFrame(tick);
  });

  // Long tasks (Chromium only): count and worst in the last minute.
  const longs = [];
  let po = null;
  if (PerformanceObserver.supportedEntryTypes?.includes('longtask')) {
    po = new PerformanceObserver((list) => { for (const e of list.getEntries()) longs.push({ at: e.startTime, ms: e.duration }); });
    po.observe({ type: 'longtask', buffered: true });
  }

  const reqs = [];
  const onReq = (r) => { reqs.unshift({ ...r, at: Date.now() }); if (reqs.length > 8) reqs.pop(); paintReqs(); };
  timingHooks.add(onReq);
  function paintReqs() {
    mount(reqList, reqs.map((r) => h('li', null,
      h('span', { class: 'hud-path truncate', title: `${r.method} ${r.path}` }, `${r.method === 'GET' ? '' : `${r.method} `}${r.path.split('?')[0]}`),
      h('span', { class: 'num', title: 'client' }, formatMs(r.clientMs ?? 0)),
      h('span', { class: 'num faint', title: 'server (Server-Timing)' }, r.serverMs != null ? formatMs(r.serverMs) : '—'))));
  }

  const timer = setInterval(() => {
    if (frames.length > 10) {
      const s = [...frames].sort((a, b) => a - b);
      const p = (q) => s[Math.min(s.length - 1, Math.floor(q * s.length))];
      frameEl.textContent = `${p(0.5).toFixed(1)} / ${p(0.95).toFixed(1)} ms · ${Math.round(1000 / p(0.5))} fps`;
      frameEl.classList.toggle('bad', p(0.95) > 20);
    }
    if (po) {
      const since = performance.now() - 60_000;
      while (longs.length && longs[0].at < since) longs.shift();
      const worst = longs.reduce((m, x) => Math.max(m, x.ms), 0);
      longEl.textContent = longs.length ? `${longs.length} · worst ${formatMs(worst)}` : '0 in the last minute';
      longEl.classList.toggle('bad', worst > 50);
    } else longEl.textContent = 'not reported by this browser';
    const mem = performance.memory;
    heapEl.textContent = mem ? `${(mem.usedJSHeapSize / 1048576).toFixed(1)} MB` : 'not reported by this browser';
  }, 500);

  return {
    destroy() {
      cancelAnimationFrame(raf);
      clearInterval(timer);
      po?.disconnect();
      timingHooks.delete(onReq);
      el.remove();
    },
  };
}

function bootMarks() {
  const m = (n) => performance.getEntriesByName(n)[0]?.startTime;
  const shell = m('ferro:shell');
  const ready = m('ferro:ready');
  return ready ? `shell ${formatMs(shell || 0)} · ready ${formatMs(ready)}` : '—';
}
