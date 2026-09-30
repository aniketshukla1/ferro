// Background updates (API.md § 13): check now, install, and restart into the new build. The
// server re-runs itself with the same port and token; this page waits for the new process,
// then reloads. Loads on first use.
import { h } from '../core/dom.js';
import { request } from '../core/api.js';
import { store } from '../core/store.js';
import { toast } from '../ui/overlay.js';
import { openDialog } from '../ui/dialog.js';

const set = (st) => store.set('update', { ...store.get('update'), ...st });

export async function checkNow() {
  try {
    const st = await request('update/check', { method: 'POST', body: {} });
    set(st);
    if (st.state === 'available') {
      toast({ title: `ferro ${st.latest} is available`, message: st.canInstall ? 'Install it from the status bar, or turn on Settings → Updates → Update in the background.' : 'Update the app from its installer.', timeout: 6000 });
    } else if (st.state === 'current') {
      toast({ kind: 'ok', title: `ferro ${st.current} is the latest version`, timeout: 2500 });
    } else if (st.state === 'downloading') {
      toast({ title: `Downloading ferro ${st.latest}…`, timeout: 2500 });
    }
  } catch (e) {
    toast({ kind: 'error', title: e.status === 422 ? 'No update host is set up' : 'Update check failed', message: e.status === 422 ? 'Set FERRO_UPDATE_MANIFEST_URL where ferro runs.' : e.message });
  }
}

export async function install() {
  try {
    set(await request('update/install', { method: 'POST', body: {} }));
  } catch (e) {
    toast({ kind: 'error', title: 'Could not install the update', message: e.message });
  }
}

export function restartNow() {
  const st = store.get('update') || {};
  openDialog({
    title: `Restart to run ferro ${st.latest || 'update'}?`,
    body: h('p', null, 'ferro restarts on the same address. Open tabs reconnect in a few seconds; nothing you have open is lost.'),
    actions: [{ label: 'Later' }, {
      label: 'Restart',
      primary: true,
      run: async () => {
        const t0 = Date.now();
        try {
          await request('update/restart', { method: 'POST', body: {} });
        } catch (e) {
          toast({ kind: 'error', title: 'Could not restart', message: e.message });
          return false;
        }
        toast({ title: 'Restarting ferro…', timeout: 15000 });
        waitForNewServer(t0);
        return true;
      },
    }],
  });
}

/** Poll until a process that started after `t0` answers (its uptime is shorter), then reload. */
function waitForNewServer(t0) {
  const tick = async () => {
    try {
      const m = await request('metrics');
      if (m.uptimeMs < Date.now() - t0) { location.reload(); return; }
    } catch { /* down between the two processes */ }
    if (Date.now() - t0 < 60_000) setTimeout(tick, 500);
    else toast({ kind: 'error', title: 'ferro did not come back', message: 'Start it again from your terminal.' });
  };
  setTimeout(tick, 700);
}
