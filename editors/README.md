# ferro in your editor

Every editor integration uses one command:

```bash
ferro open <folder | file | file:line> [--view changes|checks|history|diff]
```

It opens the location in the ferro that is already serving that folder (same browser tab
session, no second server). When none is running, it starts one, like `ferro <file:line>`.
`--no-serve` exits with code 3 instead, and `--print` prints the link instead of opening it.

## VS Code

Commands (Command Palette, the editor's title bar and right-click menu, the Source Control
title bar):

| Command | Opens |
|---|---|
| **Open in ferro** | the file at the caret line |
| **Show Diff in ferro** | the file's diff |
| **Review Changes in ferro** | the Changes panel for the folder |
| **Check This Change in ferro** | the Checks tab: breaking changes, tests, security, coverage |
| **Show History in ferro** | the History panel |

When no ferro is running for the folder, a terminal named **ferro** starts one; stop it like
any terminal. Setting `ferro.path` points at the executable when it is not on your `PATH`.

Install from this repository (no build step):

```bash
ln -s "$(pwd)/editors/vscode" ~/.vscode/extensions/ferro
```

Then reload VS Code (Developer: Reload Window). Cursor and other VS Code forks use their own
extensions folder, for example `~/.cursor/extensions`.

## JetBrains IDEs (IntelliJ, GoLand, PyCharm, WebStorm, RustRover, …)

**Settings → Tools → External Tools → +**:

| Field | Value |
|---|---|
| Name | Open in ferro |
| Program | `ferro` (or its full path) |
| Arguments | `open $FilePath$:$LineNumber$` |
| Working directory | `$ProjectFileDir$` |

Add a second tool with `open $ProjectFileDir$ --view checks` for **Check This Change**. Give
them shortcuts under **Settings → Keymap → External Tools**.

## Vim / Neovim, Zed, Sublime Text, Emacs

Map a key to the same command, for example in Neovim:

```lua
vim.keymap.set('n', '<leader>fo', function()
  vim.fn.jobstart({ 'ferro', 'open', vim.fn.expand('%:p') .. ':' .. vim.fn.line('.') }, { detach = true })
end)
```
