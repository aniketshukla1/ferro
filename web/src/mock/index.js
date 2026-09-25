// Mock mode entry: ?mock=1 (or ?mock=big for a 60k-file synthetic tree, ?mock=auth for the 401 screen).
import { setTransport } from '../core/api.js';
import { createMockServer } from './server.js';

export function installMock(mode) {
  const server = createMockServer({ big: mode === 'big', unauthorized: mode === 'auth' });
  setTransport({
    request: server.request,
    events: server.events,
    rawUrl: server.rawUrl,
  });
  window.__ferroMock = server; // for e2e tests and devtools
  return server;
}
