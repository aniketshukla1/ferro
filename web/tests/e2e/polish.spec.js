import { test, expect } from '@playwright/test';

// F6 polish (FRONTEND.md § 6.16–6.18, § 9): settings as JSON and the new ui.* keys, first-run
// tips, the latency HUD, and vim keys in the code viewer.

async function boot(page, query = '') {
  await page.goto(`/web/index.html?mock=1${query}`);
  await expect(page.locator('#app')).not.toHaveAttribute('aria-busy', 'true', { timeout: 10_000 });
}

async function putSettings(page, values) {
  await page.evaluate(async (v) => {
    const { api } = await import('/web/src/core/api.js');
    const { store } = await import('/web/src/core/store.js');
    const res = await api.putSettings(v);
    store.set('settings', res.values);
  }, values);
}

test.describe('Milestone F6 (Polish)', () => {
  test('settings JSON editor: each section has its own JSON; validates, saves, and resets removed keys (mock)', async ({ page }) => {
    await boot(page);
    await putSettings(page, { 'ui.autoReveal': false, 'ui.codeFontSize': 14, 'ui.fileIconColors': true });
    await page.keyboard.press('ControlOrMeta+Comma');
    const dialog = page.locator('.settings-dialog');
    const section = (name) => dialog.locator('.set-nav button', { hasText: name }).click();
    await dialog.locator('.set-json-btn').click();
    const ta = dialog.locator('.set-json');
    // Appearance: only its own keys.
    await expect(ta).toHaveValue(/"ui.codeFontSize": 14/);
    await expect(ta).not.toHaveValue(/ui.autoReveal/);
    // Another section shows its own JSON (it used to keep showing the first one).
    await section('Editor');
    await expect(ta).toHaveValue(/"ui.autoReveal": false/);
    await expect(ta).not.toHaveValue(/ui.codeFontSize/);

    await ta.fill('{ "ui.diffLayout": "sideways" }');
    await expect(dialog.locator('.set-json-problems li.error')).toContainText('ui.diffLayout');
    await expect(dialog.locator('.set-json-actions button', { hasText: 'Save' })).toBeDisabled();

    await ta.fill('{ "ui.wrap": true, "not.a.key": 1 ');
    await expect(dialog.locator('.set-json-problems li.error')).toBeVisible(); // JSON syntax error

    // Unsaved edits wait while you look at another section.
    await ta.fill('{ "ui.wrap": true, "ui.codeFontSize": 15, "made.up": true }');
    await section('Appearance');
    await expect(ta).toHaveValue(/"ui.codeFontSize": 14/);
    await section('Editor');
    await expect(ta).toHaveValue(/"made.up": true/);
    await expect(dialog.locator('.set-json-problems li.warn', { hasText: 'made.up' })).toBeVisible();
    await expect(dialog.locator('.set-json-problems li.warn', { hasText: 'ui.codeFontSize' })).toContainText('Appearance setting');
    await dialog.locator('.set-json-actions button', { hasText: 'Save' }).click();
    await expect(page.locator('.toast', { hasText: /Saved 4 changes/ })).toBeVisible();
    // Font size applied live; the removed key went back to its default; other sections kept theirs.
    await expect.poll(() => page.evaluate(() => getComputedStyle(document.documentElement).getPropertyValue('--code-fs').trim())).toBe('15px');
    const s = await page.evaluate(async () => (await import('/web/src/core/store.js')).store.get('settings'));
    expect(s['ui.autoReveal']).toBeUndefined();
    expect(s['ui.wrap']).toBe(true);
    expect(s['ui.fileIconColors']).toBe(true);
  });

  test('ui.* keys: line height, ruler, wrap by default, diff layout (mock)', async ({ page }) => {
    await boot(page, '&path=web/src/core/bus.js');
    await putSettings(page, { 'ui.codeFontSize': 14, 'ui.codeLineHeight': 150, 'ui.overviewRuler': false, 'ui.wrap': true, 'ui.diffLayout': 'unified' });
    await expect.poll(() => page.evaluate(() => getComputedStyle(document.documentElement).getPropertyValue('--code-lh').trim())).toBe('21px');
    await expect(page.locator('html')).toHaveClass(/no-ruler/);
    await expect(page.locator('.cv-ruler').first()).toBeHidden();
    // A file opened now starts wrapped.
    await page.locator('.cmdbar').click();
    await page.locator('.pal-input').fill('store.js');
    await page.keyboard.press('Enter');
    await expect(page.locator('.crumb.last')).toHaveText('store.js');
    await expect(page.locator('.view:not([hidden]) .cv.wrapped')).toBeVisible();
    // The diff view follows the saved layout.
    await page.keyboard.press('ControlOrMeta+KeyD');
    await expect(page.locator('.diff-layout-btn[data-layout="unified"]')).toHaveClass(/active/);
  });

  test('first-run tips show once and stay dismissed (mock)', async ({ page }) => {
    await boot(page);
    await page.evaluate(() => localStorage.removeItem('ferro.tips.dismissed'));
    await page.reload();
    await expect(page.locator('#app')).not.toHaveAttribute('aria-busy', 'true', { timeout: 10_000 });
    const tips = page.locator('.home-tips');
    await expect(tips).toBeVisible();
    await expect(tips.locator('.home-tip')).toHaveCount(4);
    await tips.locator('.ht-action', { hasText: 'Open the palette' }).click();
    await expect(page.locator('.pal-input')).toBeVisible();
    await page.keyboard.press('Escape');
    await tips.locator('.home-tips-x').click();
    await expect(tips).toHaveCount(0);
    await page.reload();
    await expect(page.locator('#app')).not.toHaveAttribute('aria-busy', 'true', { timeout: 10_000 });
    await expect(page.locator('.home-inner')).toBeVisible();
    await expect(page.locator('.home-tips')).toHaveCount(0);
  });

  test('Mod+Alt+P toggles the latency HUD and remembers it (mock)', async ({ page }) => {
    await boot(page);
    await page.keyboard.press('ControlOrMeta+Alt+KeyP');
    const hud = page.locator('.hud');
    await expect(hud).toBeVisible();
    await expect(hud).toContainText('Boot');
    // Requests show up with client and server times.
    await page.locator('.cmdbar').click();
    await page.locator('.pal-input').fill('main');
    await expect(hud.locator('.hud-reqs li', { hasText: 'fuzzy' }).first()).toBeVisible();
    await page.keyboard.press('Escape');
    const saved = await page.evaluate(async () => (await import('/web/src/core/store.js')).store.get('settings')['ui.hud']);
    expect(saved).toBe(true);
    await hud.locator('button[aria-label="Close latency HUD"]').click();
    await expect(hud).toHaveCount(0);
  });

  test('vim keys: counts, gg/G, marks, visual line (mock)', async ({ page }) => {
    await boot(page, '&path=web/src/features/palette.js&line=1');
    await putSettings(page, { 'ui.keymap': 'vim' });
    await expect(page.locator('.vim-mode')).toHaveText('NORMAL');
    await page.locator('.cv-row.cur .cv-code').click();
    const ln = () => page.locator('.cv-row.cur .cv-ln').textContent();
    await page.keyboard.type('5j');
    await expect.poll(ln).toBe('6');
    await page.keyboard.type('ma');
    await page.keyboard.type('G');
    await expect.poll(async () => Number(await ln())).toBeGreaterThan(100);
    await page.keyboard.type("'a");
    await expect.poll(ln).toBe('6');
    await page.keyboard.type('gg');
    await expect.poll(ln).toBe('1');
    await page.keyboard.type('V2j');
    await expect(page.locator('.vim-mode')).toHaveText('VISUAL LINE');
    await expect(page.locator('.cv-row.sel')).toHaveCount(3);
    await page.keyboard.press('Escape');
    await expect(page.locator('.vim-mode')).toHaveText('NORMAL');
    await expect(page.locator('.cv-row.sel')).toHaveCount(0);
    // Off again: keys go back to the viewer.
    await putSettings(page, { 'ui.keymap': 'default' });
    await expect(page.locator('.vim-mode')).toHaveCount(0);
  });
});
