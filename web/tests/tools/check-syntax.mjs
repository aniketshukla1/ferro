#!/usr/bin/env node
// Parse every ES module under web/src and web/tests. `node --check file.js` does not report ESM
// syntax errors in .js files, so each file is fed to `node --input-type=module --check` on stdin.
// Usage: node web/tests/tools/check-syntax.mjs
import { spawnSync } from 'node:child_process';
import { readdirSync, readFileSync, statSync } from 'node:fs';
import { join, relative, resolve, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '../../..');
const files = [];
function walk(dir) {
  for (const name of readdirSync(dir)) {
    const p = join(dir, name);
    if (statSync(p).isDirectory()) walk(p);
    else if (/\.m?js$/.test(name)) files.push(p);
  }
}
walk(join(root, 'web/src'));
walk(join(root, 'web/tests'));

let failed = 0;
for (const f of files) {
  const r = spawnSync(process.execPath, ['--input-type=module', '--check'], { input: readFileSync(f), encoding: 'utf8' });
  if (r.status !== 0) {
    failed++;
    console.error(`::error file=${relative(root, f)}::syntax error\n${r.stderr}`);
  }
}
console.log(`${files.length - failed}/${files.length} modules parse`);
process.exit(failed ? 1 : 0);
