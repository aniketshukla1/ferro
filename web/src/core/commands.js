// Command registry: every user action is a command. The palette lists them,
// shortcuts run them, menus reference them by id.
import { bindKey } from './keys.js';

const commands = new Map();

/**
 * @typedef {Object} Command
 * @property {string} id
 * @property {string} title
 * @property {string} [category]   e.g. "View", "Go", "Git"
 * @property {string} [icon]
 * @property {string[]} [keys]     shortcut specs (first one is shown)
 * @property {string[]} [desktopKeys] extra shortcuts only in the desktop host
 * @property {() => boolean} [when]
 * @property {boolean} [inInput]   shortcut also fires while typing
 * @property {boolean} [hidden]    not listed in the palette
 * @property {(...args:any[]) => any} run
 */

/** @param {Command} cmd */
export function command(cmd) {
  commands.set(cmd.id, cmd);
  for (const spec of cmd.keys || []) bindKey(spec, () => execute(cmd.id), { when: cmd.when, inInput: cmd.inInput, id: cmd.id });
  for (const spec of cmd.desktopKeys || []) bindKey(spec, () => execute(cmd.id), { when: cmd.when, inInput: cmd.inInput, id: cmd.id, host: 'desktop' });
  return cmd;
}

export function execute(id, ...args) {
  const cmd = commands.get(id);
  if (!cmd) {
    console.warn('[commands] unknown', id);
    return undefined;
  }
  if (cmd.when && !cmd.when()) return undefined;
  return cmd.run(...args);
}

export const getCommand = (id) => commands.get(id);

/** Commands available right now, for the palette. */
export function listCommands() {
  return [...commands.values()].filter((c) => !c.hidden && (!c.when || c.when()));
}
