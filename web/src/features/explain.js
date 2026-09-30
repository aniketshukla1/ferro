// AI change notes (API.md § 10.8): on request, each file in the open diff gets a one-line summary
// and every hunk a tag (Added / Removed / Changed) with a plain-words note on what it does. The
// server caches notes by diff text, so reopening a commit is free. Loads on first use.
import { request } from '../core/api.js';
import { plural } from '../core/util.js';
import { toast } from '../ui/overlay.js';

const MAX_FILES = 40;
const PARALLEL = 3;
let running = null;

export async function explainOpenDiff(dv) {
  if (running) return running;
  const base = dv.getBase();
  const target = dv.getTarget();
  // The file on screen first (so its notes show up at once), then the rest in list order.
  const active = dv.getActivePath?.();
  const files = (dv.getChangeset()?.files || [])
    .filter((f) => !f.binary)
    .sort((a, b) => (b.path === active) - (a.path === active))
    .slice(0, MAX_FILES);
  if (!files.length) {
    toast({ title: 'Nothing to explain', message: 'Open a diff with text changes first.', timeout: 2500 });
    return null;
  }
  const hunks = new Map();
  const summaries = new Map();
  let done = 0;
  let failed = 0;
  let fatal = null;
  const progress = toast({ title: `Explaining ${plural(files.length, 'file')}…`, message: `0 of ${files.length}`, timeout: 0 });
  const queue = [...files];
  const worker = async () => {
    while (queue.length && !fatal) {
      const f = queue.shift();
      try {
        const res = await request('ai/explain', { method: 'POST', body: { path: f.path, base, target } });
        if (res.summary) summaries.set(f.path, res.summary);
        for (const n of res.hunks || []) hunks.set(`${f.path}\n${n.id}`, { kind: n.kind, note: n.note });
        if (dv.getBase() === base && dv.getTarget() === target) dv.setNotes({ hunks, files: summaries });
      } catch (e) {
        // No provider (or it is off): stop at once instead of failing every file.
        if (e.status === 422 && /provider|not configured|AI/i.test(e.message)) fatal = e;
        else failed++;
      }
      done++;
      progress.update({ message: `${done} of ${files.length}` });
    }
  };
  running = Promise.all(Array.from({ length: PARALLEL }, worker)).then(() => {
    progress.close();
    running = null;
    if (fatal) {
      toast({ kind: 'error', title: 'AI is not set up', message: fatal.message });
    } else {
      const extra = (dv.getChangeset()?.files.length || 0) > MAX_FILES ? ` (the first ${MAX_FILES})` : '';
      toast({ kind: failed ? 'warn' : 'ok', title: `Explained ${plural(done - failed, 'file')}${extra}`, message: failed ? `${plural(failed, 'file')} could not be explained.` : undefined, timeout: 3000 });
    }
  });
  return running;
}
