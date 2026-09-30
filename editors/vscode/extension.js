// ferro for VS Code: open the current file, its diff, your changes, Checks or History in ferro.
// Everything goes through `ferro open`, which reuses the ferro already serving the folder; when
// none is running, a "ferro" terminal starts one (visible, and stopped like any terminal).
'use strict';
const vscode = require('vscode');
const { execFile } = require('child_process');
const { NO_SERVER, target, openArgs, quote } = require('./lib');

const ferroPath = () => vscode.workspace.getConfiguration('ferro').get('path') || 'ferro';

/** The file (with the caret line when it is the active editor) and its workspace folder. */
function here(uri) {
  const ed = vscode.window.activeTextEditor;
  const res = uri instanceof vscode.Uri ? uri : ed?.document?.uri;
  const folder = (res && vscode.workspace.getWorkspaceFolder(res)) || vscode.workspace.workspaceFolders?.[0];
  const at = { folder: folder?.uri.fsPath };
  if (res?.scheme === 'file') {
    at.file = res.fsPath;
    if (ed && ed.document.uri.toString() === res.toString()) at.line = ed.selection.active.line + 1;
  }
  return at;
}

function run(view, uri) {
  const at = here(uri);
  const tgt = target(at, view);
  if (!tgt) {
    vscode.window.showInformationMessage('ferro: open a folder or a file first.');
    return;
  }
  const cwd = at.folder;
  execFile(ferroPath(), openArgs(tgt, view, { noServe: true }), { cwd, timeout: 15000 }, (err) => {
    if (!err) return;
    if (err.code === NO_SERVER) {
      const term = vscode.window.createTerminal({ name: 'ferro', cwd });
      const win = process.platform === 'win32';
      term.sendText([ferroPath(), ...openArgs(tgt, view)].map((a) => quote(a, win)).join(' '));
      term.show(true);
      return;
    }
    const missing = err.code === 'ENOENT';
    vscode.window
      .showErrorMessage(missing ? 'ferro is not installed or not on your PATH.' : `ferro open failed: ${err.message}`, ...(missing ? ['Set ferro.path'] : []))
      .then((pick) => { if (pick) vscode.commands.executeCommand('workbench.action.openSettings', 'ferro.path'); });
  });
}

function activate(context) {
  const commands = {
    'ferro.openFile': undefined,
    'ferro.showDiff': 'diff',
    'ferro.reviewChanges': 'changes',
    'ferro.checkChange': 'checks',
    'ferro.showHistory': 'history',
  };
  for (const [id, view] of Object.entries(commands)) {
    context.subscriptions.push(vscode.commands.registerCommand(id, (uri) => run(view, uri)));
  }
}

module.exports = { activate, deactivate() {} };
