// Pure helpers for the ferro VS Code extension (no `vscode` import), tested with node.
'use strict';

/** `ferro open` exits with this code when no server is running and `--no-serve` was given. */
const NO_SERVER = 3;

/** Views that show the whole folder rather than one file. */
const FOLDER_VIEWS = new Set(['changes', 'checks', 'history']);

/**
 * What to hand `ferro open`: `file:line` for the file views, the folder for the others.
 * @param {{file?: string, line?: number, folder?: string}} at
 * @param {string} [view]
 */
function target(at, view) {
  if (view && FOLDER_VIEWS.has(view)) return at.folder || at.file || null;
  if (at.file) return at.line ? `${at.file}:${at.line}` : at.file;
  return at.folder || null;
}

/** Arguments for `ferro open`. */
function openArgs(tgt, view, { noServe = false } = {}) {
  const args = ['open', tgt];
  if (view) args.push('--view', view);
  if (noServe) args.push('--no-serve');
  return args;
}

/** One argument quoted for a terminal: POSIX shells, or PowerShell on Windows. */
function quote(s, windows = false) {
  if (/^[\w@%+=:,./\\-]+$/.test(s)) return s;
  return windows ? `'${s.replace(/'/g, "''")}'` : `'${s.replace(/'/g, "'\\''")}'`;
}

module.exports = { NO_SERVER, target, openArgs, quote };
