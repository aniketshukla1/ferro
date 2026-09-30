#!/usr/bin/env node
// README media: screenshots (dark + light) and a demo walkthrough recorded frame by frame, from
// the live demo site (scripts/build-demo-site.sh) served locally. Frames are written as PNGs;
// tools/readme-gif.py turns them into docs/assets/demo.gif.
//
//   scripts/build-demo-site.sh /tmp/_site && python3 -m http.server 4190 -d /tmp/_site &
//   cd web/tests && node tools/readme-media.mjs            # DEMO_URL overrides the address
import { chromium } from '@playwright/test';
import { mkdirSync, writeFileSync, rmSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const HERE = dirname(fileURLToPath(import.meta.url));
const OUT = join(HERE, '../../../docs/assets');
const FRAMES = join(OUT, '.frames');
const BASE = process.env.DEMO_URL || 'http://127.0.0.1:4190/web/index.html';
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// The connection pill says "mock" in this mode; marketing shots leave it out.
const HIDE = '.sb-item:has(.conn-dot) { display: none !important; } .toast-host, .toasts { display: none !important; }';

async function open(browser, scheme, query, size = { width: 1600, height: 1000 }) {
  const page = await browser.newPage({ viewport: size, deviceScaleFactor: 1, colorScheme: scheme, bypassCSP: true });
  await page.goto(`${BASE}?mock=1${query ? `&${query}` : ''}`);
  await page.waitForSelector('#app:not([aria-busy="true"])', { timeout: 15000 });
  await page.addStyleTag({ content: HIDE });
  await sleep(400);
  return page;
}

const bus = (page, event, data) => page.evaluate(async ([e, d]) => (await import('/web/src/core/bus.js')).bus.emit(e, d), [event, data]);
const api = (page, path, opts) => page.evaluate(async ([p, o]) => (await import('/web/src/core/api.js')).request(p, o), [path, opts || {}]);

async function inspector(page, on) {
  const open = await page.evaluate(() => !document.querySelector('.app')?.classList.contains('no-inspector'));
  if (open !== on) await page.keyboard.press('ControlOrMeta+KeyJ');
  await sleep(250);
}

/** History → the feature/search branch → its newest commit ("fuzzy scoring by path depth"). */
async function showcaseCommit(page) {
  await page.keyboard.press('ControlOrMeta+Shift+KeyH');
  await page.waitForSelector('.hi-row');
  await page.locator('.hi-ref-sel').selectOption('feature/search');
  const row = page.locator('.hi-row', { hasText: 'fuzzy scoring by path depth' });
  await row.waitFor();
  await sleep(300);
  await row.click();
  await page.waitForSelector('.hc-bar');
  await page.waitForSelector('.diff-hunk-sep');
  await sleep(700);
}

async function shots(browser, scheme) {
  const tag = scheme === 'light' ? 'light' : 'dark';
  const save = async (page, name) => { await page.screenshot({ path: join(OUT, `screen-${name}-${tag}.png`) }); console.log(`screen-${name}-${tag}.png`); };

  // Checks on a commit: the hero shot.
  let page = await open(browser, scheme, '');
  await showcaseCommit(page);
  await bus(page, 'checks:open');
  await sleep(1500);
  await save(page, 'checks');
  if (scheme === 'light') { await page.close(); return; }

  // The commit itself, then AI change notes on it.
  await inspector(page, false);
  await sleep(300);
  await save(page, 'history');
  await page.locator('.diff-explain-btn').click();
  await page.waitForSelector('.diff-note.hunk-note', { timeout: 10000 });
  await sleep(900);
  await save(page, 'explain');
  await page.close();

  // Code with IDE colors.
  page = await open(browser, scheme, 'path=crates/ferro-core/src/fuzzy.rs&line=48');
  await inspector(page, false);
  await sleep(500);
  await save(page, 'code');
  await page.close();

  // Inline edit: the AI's proposal as a diff, before saving.
  page = await open(browser, scheme, 'path=crates/ferro-core/src/fuzzy.rs&line=26');
  await inspector(page, false);
  await page.locator('.cv-row[data-n="22"] .cv-ln').click();
  await page.keyboard.down('Shift');
  await page.locator('.cv-row[data-n="30"] .cv-ln').click();
  await page.keyboard.up('Shift');
  await page.keyboard.press('Alt+KeyK');
  await page.locator('.ie-prompt').fill('add a doc comment explaining the two paths');
  await page.keyboard.press('Enter');
  await page.waitForSelector('.ie-diff:not([hidden])');
  await sleep(700);
  await save(page, 'edit');
  await page.close();

  // Team review memory: shared rules and a suggestion.
  page = await open(browser, scheme, '');
  await api(page, 'memory/rules', { method: 'POST', body: { kind: 'ignore', appliesTo: 'security', rule: 'secret.*', paths: ['tests/**', '**/fixtures/**'], reason: 'Test fixtures use fake keys', scope: 'team' } });
  await api(page, 'memory/rules', { method: 'POST', body: { kind: 'convention', appliesTo: 'ai', text: 'Every public function has a doc comment', scope: 'team' } });
  await showcaseCommit(page);
  await bus(page, 'memory:open');
  await sleep(1200);
  await save(page, 'memory');
  await page.close();

  // The palette.
  page = await open(browser, scheme, 'path=crates/ferro-core/src/fuzzy.rs&line=48');
  await inspector(page, false);
  await page.locator('.cmdbar').click();
  await page.locator('.pal-input').pressSequentially('srv', { delay: 40 });
  await sleep(700);
  await save(page, 'palette');
  await page.close();
}

/** The walkthrough, as timed PNG frames (Chromium screencast: a frame per repaint). */
async function walkthrough(browser) {
  rmSync(FRAMES, { recursive: true, force: true });
  mkdirSync(FRAMES, { recursive: true });
  const page = await open(browser, 'dark', '', { width: 1280, height: 800 });
  const cdp = await page.context().newCDPSession(page);
  const frames = [];
  cdp.on('Page.screencastFrame', async ({ data, metadata, sessionId }) => {
    frames.push({ data, t: metadata.timestamp });
    await cdp.send('Page.screencastFrameAck', { sessionId }).catch(() => {});
  });
  // A still stretch emits no frames: the hold marker keeps its length in the GIF.
  const hold = async (ms) => { frames.push({ hold: true, t: Date.now() / 1000 }); await sleep(ms); };
  try {
    await cdp.send('Page.startScreencast', { format: 'png', everyNthFrame: 1, maxWidth: 1280, maxHeight: 800 });
    await hold(1300);
    await inspector(page, false);
    // 1. Find a file.
    await page.locator('.cmdbar').click();
    await page.locator('.pal-input').waitFor();
    await page.locator('.pal-input').pressSequentially('fuzzy.rs', { delay: 110 });
    await hold(700);
    await page.keyboard.press('Enter');
    await hold(1600);
    // 2. A commit from history.
    await showcaseCommit(page);
    await hold(1800);
    // 3. What each change does.
    await page.locator('.diff-explain-btn').click();
    await page.waitForSelector('.diff-note.hunk-note', { timeout: 10000 });
    await hold(2000);
    // 4. Does it break anything?
    await page.locator('.diff-checks-btn').click();
    await hold(2800);
    await cdp.send('Page.stopScreencast');
  } catch (e) {
    await page.screenshot({ path: join(FRAMES, 'failure.png') });
    throw e;
  } finally {
    await page.close();
  }
  const index = [];
  let n = 0;
  for (const f of frames) {
    if (f.hold) { index.push({ hold: true, t: f.t }); continue; }
    const name = `f${String(n++).padStart(4, '0')}.png`;
    writeFileSync(join(FRAMES, name), Buffer.from(f.data, 'base64'));
    index.push({ file: name, t: f.t });
  }
  writeFileSync(join(FRAMES, 'index.json'), JSON.stringify(index));
  console.log(`walkthrough: ${n} frames`);
}

/** The README banner (docs/assets/src/banner.html) over the fresh screenshots. */
async function banners(browser) {
  for (const theme of ['dark', 'light']) {
    const page = await browser.newPage({ viewport: { width: 1600, height: 560 }, deviceScaleFactor: 1 });
    await page.goto(`${pathToFileURL(join(OUT, 'src/banner.html')).href}?theme=${theme}`);
    await page.waitForFunction(() => document.getElementById('shot').complete);
    await sleep(300);
    await page.screenshot({ path: join(OUT, `banner-${theme}.png`) });
    console.log(`banner-${theme}.png`);
    await page.close();
  }
}

mkdirSync(OUT, { recursive: true });
const browser = await chromium.launch();
try {
  const only = process.argv[2];
  if (!only || only === 'shots') {
    await shots(browser, 'dark');
    await shots(browser, 'light');
  }
  if (!only || only === 'banner') await banners(browser);
  if (!only || only === 'walkthrough') await walkthrough(browser);
} finally {
  await browser.close();
}
