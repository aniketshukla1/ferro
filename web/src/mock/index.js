// Mock mode entry: ?mock=1 (or ?mock=big for a 60k-file synthetic tree, ?mock=auth for the 401 screen).
// ?demo=1 adds the public-demo ribbon (the GitHub Pages demo links here).
import { setTransport } from '../core/api.js';
import { h } from '../core/dom.js';
import { createMockServer } from './server.js';

export function installMock(mode) {
  const server = createMockServer({ big: mode === 'big', unauthorized: mode === 'auth' });
  setTransport({
    request: server.request,
    events: server.events,
    stream: server.stream,
    rawUrl: server.rawUrl,
  });
  window.__ferroMock = server; // for e2e tests and devtools
  if (new URLSearchParams(location.search).has('demo')) showDemoRibbon();
  return server;
}

/** "Live demo with sample data" + install link, dismissible for the session. */
function showDemoRibbon() {
  try { if (sessionStorage.getItem('ferro.demoRibbon') === 'off') return; } catch { /* private mode */ }
  const ribbon = h('div', { class: 'demo-ribbon', role: 'note' },
    h('span', { class: 'demo-dot', 'aria-hidden': 'true' }),
    h('span', null, h('strong', null, 'Live demo'), ' with sample data. Nothing you do here is saved.'),
    h('a', { class: 'demo-cta', href: 'https://github.com/aniketshukla1/ferro#-quickstart', target: '_blank', rel: 'noopener noreferrer' }, 'Install ferro →'),
    h('button', {
      class: 'demo-close', type: 'button', 'aria-label': 'Hide the demo notice',
      on: { click: () => { ribbon.remove(); try { sessionStorage.setItem('ferro.demoRibbon', 'off'); } catch { /* ignore */ } } },
    }, '×'));
  const mount = () => document.body.appendChild(ribbon);
  if (document.body) mount();
  else addEventListener('DOMContentLoaded', mount, { once: true });
}
