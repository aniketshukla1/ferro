#!/usr/bin/env node
// Generate web/styles/themes-extra.css (the 13 lazily loaded F6 themes) from the graphite and
// porcelain blocks in themes.css. Each theme re-tints the template's neutrals (surfaces, text,
// comments, punctuation, plain identifiers) to one hue, keeps the template's IDE syntax colors
// and git/diff state, then nudges any foreground that misses its § 5.2 contrast floor.
// Usage: node web/tests/tools/gen-themes.mjs   (then run the contrast unit test)
import { readFileSync, writeFileSync } from 'node:fs';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { contrastRules } from '../unit/contrast-rules.js';

const STYLES = join(dirname(fileURLToPath(import.meta.url)), '../../styles');
const base = readFileSync(join(STYLES, 'themes.css'), 'utf8');

function block(name) {
  const m = base.match(new RegExp(`\\[data-theme="${name}"\\]\\s*\\{([^}]*)\\}`));
  const vars = {};
  for (const v of m[1].matchAll(/--([\w-]+):\s*([^;]+);/g)) vars[v[1]] = v[2].trim();
  return vars;
}

// id, name, type, note, neutral hue, neutral saturation, surface lightness shift,
// string hue, number hue, text-lightness scale (high contrast)
const THEMES = [
  { id: 'slate', name: 'Slate', type: 'dark', note: 'cool blue-grey', hue: 216, sat: 0.13, lift: 0.035 },
  { id: 'fjord', name: 'Fjord', type: 'dark', note: 'arctic, softer dark', hue: 214, sat: 0.18, lift: 0.1, str: 140, num: 38 },
  { id: 'abyss', name: 'Abyss', type: 'dark', note: 'deep navy', hue: 226, sat: 0.38, lift: 0.005 },
  { id: 'moss', name: 'Moss', type: 'dark', note: 'green-grey', hue: 150, sat: 0.1, lift: 0.025, str: 105 },
  { id: 'umber', name: 'Umber', type: 'dark', note: 'warm brown-black', hue: 28, sat: 0.13, lift: 0.025, str: 95, num: 30 },
  { id: 'dusk', name: 'Dusk', type: 'dark', note: 'violet-grey', hue: 262, sat: 0.13, lift: 0.035 },
  { id: 'paper', name: 'Paper', type: 'light', note: 'warm off-white', hue: 42, sat: 0.3, lift: -0.02 },
  { id: 'mist', name: 'Mist', type: 'light', note: 'cool grey', hue: 216, sat: 0.14, lift: -0.04 },
  { id: 'sand', name: 'Sand', type: 'light', note: 'sepia, low glare', hue: 38, sat: 0.36, lift: -0.075, str: 100, num: 25 },
  { id: 'sage', name: 'Sage', type: 'light', note: 'green-tinted', hue: 110, sat: 0.12, lift: -0.03 },
  { id: 'frost', name: 'Frost', type: 'light', note: 'pale blue', hue: 205, sat: 0.3, lift: -0.02 },
  { id: 'dawn', name: 'Dawn', type: 'light', note: 'rose-tinted', hue: 340, sat: 0.16, lift: -0.02 },
  { id: 'chalk', name: 'Chalk', type: 'light', note: 'highest contrast', hue: 0, sat: 0, lift: 0, text: 0.55 },
];

const NEUTRAL_SURFACE = ['bg-chrome', 'bg-0', 'bg-1', 'bg-2', 'bg-3', 'bg-inset', 'border', 'border-strong'];
// Syntax that stays neutral (tinted to the theme hue); every other syn-* keeps its template hue.
const NEUTRAL_SYNTAX = ['syn-comment', 'syn-doc', 'syn-punct', 'syn-variable'];
const STRING = ['syn-string', 'syn-escape', 'syn-regex', 'syn-inserted'];
const NUMBER = ['syn-number', 'syn-constant'];
const FIXED = ['sel-tint', 'ok', 'warn', 'danger', 'info', 'syn-link', 'syn-deleted', 'syn-error'];

function toHsl(hex) {
  const [r, g, b] = [1, 3, 5].map((i) => parseInt(hex.slice(i, i + 2), 16) / 255);
  const max = Math.max(r, g, b), min = Math.min(r, g, b), l = (max + min) / 2, d = max - min;
  if (!d) return [0, 0, l];
  const s = d / (1 - Math.abs(2 * l - 1));
  const h = max === r ? ((g - b) / d) % 6 : max === g ? (b - r) / d + 2 : (r - g) / d + 4;
  return [(h * 60 + 360) % 360, s, l];
}
function toHex([h, s, l]) {
  l = Math.min(1, Math.max(0, l));
  s = Math.min(1, Math.max(0, s));
  const c = (1 - Math.abs(2 * l - 1)) * s, x = c * (1 - Math.abs(((h / 60) % 2) - 1)), m = l - c / 2;
  const [r, g, b] = h < 60 ? [c, x, 0] : h < 120 ? [x, c, 0] : h < 180 ? [0, c, x] : h < 240 ? [0, x, c] : h < 300 ? [x, 0, c] : [c, 0, x];
  return '#' + [r, g, b].map((v) => Math.round((v + m) * 255).toString(16).padStart(2, '0')).join('');
}

function build(t) {
  const tpl = block(t.type === 'dark' ? 'graphite' : 'porcelain');
  const out = {};
  for (const [k, v] of Object.entries(tpl)) {
    if (!v.startsWith('#')) { out[k] = v; continue; }
    const [h, s, l] = toHsl(v);
    if (NEUTRAL_SURFACE.includes(k)) out[k] = toHex([t.hue, t.sat, l + t.lift]);
    else if (FIXED.includes(k)) out[k] = v;
    else if (STRING.includes(k)) out[k] = toHex([t.str ?? h, s, l]);
    else if (NUMBER.includes(k)) out[k] = toHex([t.num ?? h, s, l]);
    else if (k.startsWith('syn-') && !NEUTRAL_SYNTAX.includes(k)) out[k] = v;
    else out[k] = toHex([t.hue, t.sat, t.text ? l * t.text : l]);
  }
  out['accent'] = out['fg'];
  out['accent-fg'] = t.type === 'dark' ? out['bg-chrome'] : '#ffffff';
  // Nudge foregrounds toward the far end of the scale until every rule passes with a margin.
  const dir = t.type === 'dark' ? 1 : -1;
  for (let pass = 0; pass < 3; pass++) {
    for (const rule of contrastRules(out)) {
      let [h, s, l] = toHsl(out[rule.fg]);
      for (let i = 0; i < 200 && rule.min(out) < rule.need + 0.08; i++) {
        l += dir * 0.004;
        out[rule.fg] = toHex([h, s, l]);
      }
    }
  }
  return out;
}

const ORDER = ['color-scheme', ...Object.keys(block('graphite')).filter((k) => k !== 'color-scheme')];
let css = `/* ferro F6 themes, loaded on demand (features/themes.js, boot-theme.js); not part of the boot CSS.
   Generated by web/tests/tools/gen-themes.mjs from the graphite / porcelain templates in themes.css;
   edit the generator, not this file. Same token set, same contrast test (unit/contrast.test.js). */\n`;
for (const t of THEMES) {
  const v = build(t);
  css += `\n/* ---------- ${t.id} (${t.type}, ${t.note}) ---------- */\n[data-theme="${t.id}"] {\n`;
  for (const k of ORDER) {
    if (k === 'syn-comment') css += '\n';
    css += `  ${k === 'color-scheme' ? 'color-scheme' : '--' + k}: ${k === 'color-scheme' ? t.type : v[k]};\n`;
  }
  css += '}\n';
}
writeFileSync(join(STYLES, 'themes-extra.css'), css);
console.log(`wrote ${THEMES.length} themes`);
