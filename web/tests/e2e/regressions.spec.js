// Regressions from the 2026-09-27 review of main (F1 open items + F2): each test pins a bug that
// shipped with the suite green.
import { test, expect } from '@playwright/test';

const boot = async (page, query = '') => {
  await page.goto(`/web/index.html?mock=1${query}`);
  await expect(page.locator('#app')).not.toHaveAttribute('aria-busy', 'true', { timeout: 15_000 });
};

test('opening the diff view keeps the editor: files render after it closes', async ({ page }) => {
  await boot(page, '&path=README.md');
  await page.evaluate(async () => (await import('./src/core/bus.js')).bus.emit('diff:open', { path: 'crates/ferro-core/src/search.rs' }));
  await expect(page.locator('.diff-view')).toBeVisible();
  // Cmd/Ctrl+C inside the diff copies (the `c` comment shortcut is for the plain key only).
  const prevented = await page.evaluate(() => {
    const ev = new KeyboardEvent('keydown', { key: 'c', metaKey: true, ctrlKey: true, bubbles: true, cancelable: true });
    document.querySelector('.diff-scroller').dispatchEvent(ev);
    return ev.defaultPrevented;
  });
  expect(prevented).toBe(false);
  await page.locator('.diff-close-btn').click();
  await page.evaluate(async () => (await import('./src/core/bus.js')).bus.emit('diff:open', { path: 'crates/ferro-core/src/search.rs' }));
  await page.locator('.diff-close-btn').click();
  await page.keyboard.press(process.platform === 'darwin' ? 'Meta+p' : 'Control+p');
  await page.locator('.pal-input').fill('search.rs');
  // Enter picks the first result; wait until results reflect the query, not the empty-query list.
  await expect(page.locator('.pal-item').first()).toContainText('search.rs');
  await page.keyboard.press('Enter');
  await expect(page.locator('.cv-row:not(.skel)').first()).toBeVisible();
});

test('find runs as you type and Enter searches a changed query', async ({ page }) => {
  await boot(page, '&path=generated/large.log');
  await expect(page.locator('.cv-row:not([hidden])').first()).toBeVisible({ timeout: 10_000 });
  await page.evaluate(async () => (await import('./src/core/commands.js')).execute('find.open'));
  const input = page.locator('.fb-input');
  const count = page.locator('.fb-count');
  await input.fill('FERRO_CANARY_512KB');
  await expect(count).toContainText('of 2'); // no Enter needed
  await input.fill('xyzzy-no-such-text');
  await input.press('Enter');
  await expect(count).toHaveText('No results');
});

test('collapsing a folder leaves its parent open', async ({ page }) => {
  await boot(page);
  await page.evaluate(async () => {
    const { execute } = await import('./src/core/commands.js');
    execute('panel.files');
    (await import('./src/core/bus.js')).bus.emit('tree:reveal', { path: 'crates/ferro-core/src/search.rs' });
  });
  const row = (name) => page.locator('.tree-row', { has: page.locator('.tname', { hasText: new RegExp(`^${name}$`) }) });
  await expect(row('ferro-core')).toHaveAttribute('aria-expanded', 'true');
  await row('ferro-core').click();
  await expect(row('ferro-core')).toHaveAttribute('aria-expanded', 'false');
  const openDirs = await page.evaluate(async () => (await import('./src/features/session.js')).session.data.openDirs);
  expect(openDirs).toContain('crates');
});

test('overview ruler jumps; word wrap never overlaps rows', async ({ page }) => {
  await boot(page, '&path=generated/large.log');
  await expect(page.locator('.cv-row:not([hidden])').first()).toBeVisible({ timeout: 10_000 });
  const errors = [];
  page.on('pageerror', (e) => errors.push(e.message));
  const ruler = page.locator('.cv-ruler').first();
  const box = await ruler.boundingBox();
  const view = await page.locator('.cv').first().boundingBox();
  expect(box.height).toBeGreaterThan(view.height - 2); // spans the code, not a 100px strip
  await ruler.click({ position: { x: 5, y: box.height * 0.5 } });
  await expect.poll(() => page.evaluate(async () => (await import('./src/core/store.js')).store.get('cursor')?.line)).toBeGreaterThan(1000);
  expect(errors).toEqual([]);

  await page.evaluate(async () => (await import('./src/core/commands.js')).execute('view.toggleWrap'));
  await page.setViewportSize({ width: 640, height: 700 });
  const layout = await page.evaluate(async () => {
    await new Promise((r) => setTimeout(r, 400));
    const rows = [...document.querySelectorAll('.cv-row:not([hidden])')]
      .map((r) => ({ n: +r.dataset.n, top: parseFloat(r.style.transform.slice(11)), h: r.offsetHeight, need: r.lastChild.scrollHeight }))
      .sort((a, b) => a.n - b.n);
    let overlaps = 0;
    for (let i = 0; i + 1 < rows.length; i++) if (rows[i + 1].n === rows[i].n + 1 && rows[i].top + rows[i].h > rows[i + 1].top + 0.5) overlaps++;
    return { rows: rows.length, overlaps, clipped: rows.filter((r) => r.need > r.h + 1).length };
  });
  expect(layout.rows).toBeGreaterThan(0);
  expect(layout.overlaps).toBe(0);
  expect(layout.clipped).toBe(0);
});
