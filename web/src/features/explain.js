// AI change notes (API.md § 10.8): on request, each file in the open diff gets a one-line summary
// and every hunk a tag (Added / Removed / Changed), a plain-words note on what it does, and a
// verdict: fine, could be better, or a problem, with why and the suggested code to apply or copy.
// The server caches notes by diff text, so reopening a commit is free. Loads on first use.
import { request, has } from '../core/api.js';
import { bus } from '../core/bus.js';
import { h } from '../core/dom.js';
import { plural } from '../core/util.js';
import { toast } from '../ui/overlay.js';
import { openDialog } from '../ui/dialog.js';

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
  let flagged = 0;
  let fatal = null;
  const progress = toast({ title: `Explaining ${plural(files.length, 'file')}…`, message: `0 of ${files.length}`, timeout: 0 });
  const queue = [...files];
  const worker = async () => {
    while (queue.length && !fatal) {
      const f = queue.shift();
      try {
        const res = await request('ai/explain', { method: 'POST', body: { path: f.path, base, target } });
        if (res.summary) summaries.set(f.path, res.summary);
        for (const n of res.hunks || []) {
          hunks.set(`${f.path}\n${n.id}`, n);
          if (n.why) flagged++;
        }
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
      const msg = [flagged && `${plural(flagged, 'change')} could be better.`, failed && `${plural(failed, 'file')} could not be explained.`].filter(Boolean).join(' ');
      toast({ kind: failed ? 'warn' : 'ok', title: `Explained ${plural(done - failed, 'file')}${extra}`, message: msg || undefined, timeout: 3000 });
    }
  });
  return running;
}

/** One change's verdict: why, and the suggested code as a diff to apply (working tree) or copy. */
async function openHunk(item) {
  const sg = item.suggestion;
  const body = h('div', { class: 'hunk-review' }, h('p', null, item.why), h('p', { class: 'faint small' }, item.note));
  if (sg) {
    const { lineDiff } = await import('./inline-edit.js');
    body.append(h('pre', { class: 'hunk-review-diff' }, lineDiff(sg.original.split('\n'), sg.code.split('\n'))
      .map((r) => h('div', { class: `hr-${r.t}` }, `${r.t === 'add' ? '+' : r.t === 'del' ? '-' : ' '} ${r.text}`))));
  }
  const apply = async () => {
    try {
      await request('file/edit', { method: 'POST', body: { path: item.path, startLine: sg.start, endLine: sg.end, expected: sg.original, text: sg.code } });
      toast({ kind: 'ok', title: 'Suggestion applied', message: item.path, timeout: 2500 });
      return true;
    } catch (e) {
      toast({ kind: 'error', title: 'Not applied', message: e.status === 409 ? 'These lines changed since the review. Explain again.' : e.message });
      return false;
    }
  };
  openDialog({
    title: item.verdict === 'problem' ? 'Problem' : 'Could be better',
    className: 'hunk-review-dialog',
    width: 'min(720px, calc(100vw - 32px))',
    body,
    actions: [{ label: 'Close' },
      sg && { label: 'Copy code', run: () => { navigator.clipboard?.writeText(sg.code); return true; } },
      sg && item.target === 'worktree' && has('file.edit') && { label: 'Apply', primary: true, run: apply }].filter(Boolean),
  });
}
bus.on('ai:hunk', openHunk);
