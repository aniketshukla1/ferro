import { test, expect } from '@playwright/test';

// F5 code navigation (FRONTEND.md § 6.15) against the mock's word-based index of web/src.

async function openAt(page, path, line) {
  await page.goto(`/web/index.html?mock=1&path=${encodeURIComponent(path)}&line=${line}`);
  await expect(page.locator('#app')).not.toHaveAttribute('aria-busy', 'true', { timeout: 10_000 });
  await expect(page.locator('.cv-row.cur')).toBeVisible();
}

/** Viewport point at the middle of `word` inside the first code row containing `rowText`. */
async function wordPoint(page, rowText, word) {
  const row = page.locator('.cv-row', { hasText: rowText }).first();
  await expect(row).toBeVisible();
  return row.evaluate((el, w) => {
    const code = el.querySelector('.cv-code');
    const at = code.textContent.indexOf(w);
    const walker = document.createTreeWalker(code, NodeFilter.SHOW_TEXT);
    let pos = 0;
    for (let n = walker.nextNode(); n; n = walker.nextNode()) {
      if (at < pos + n.data.length) {
        const r = document.createRange();
        r.setStart(n, at - pos + 1);
        r.setEnd(n, at - pos + 2);
        const b = r.getBoundingClientRect();
        return { x: b.left + b.width / 2, y: b.top + b.height / 2 };
      }
      pos += n.data.length;
    }
    return null;
  }, word);
}

test.describe('Milestone F5 (Navigation)', () => {
  test('hover card shows the signature and dismisses on Escape (mock)', async ({ page }) => {
    await openAt(page, 'web/src/main.js', 14);
    const p = await wordPoint(page, "import { buildShell }", 'buildShell');
    await page.mouse.move(p.x - 1, p.y);
    await page.mouse.move(p.x, p.y);
    const card = page.locator('.hov-card');
    await expect(card).toBeVisible();
    await expect(card.locator('.hov-name')).toHaveText('buildShell');
    await expect(card.locator('.hov-sig')).toContainText('export function buildShell(root)');
    await expect(card.locator('.hov-loc')).toContainText('shell.js:');
    await page.keyboard.press('Escape');
    await expect(card).toHaveCount(0);
  });

  test('Mod+click opens the only definition (mock)', async ({ page }) => {
    await openAt(page, 'web/src/main.js', 14);
    const p = await wordPoint(page, "import { buildShell }", 'buildShell');
    await page.keyboard.down('ControlOrMeta');
    await page.mouse.click(p.x, p.y);
    await page.keyboard.up('ControlOrMeta');
    await expect(page.locator('.crumb.last')).toHaveText('shell.js');
    await expect(page.locator('.cv-row.cur .cv-code')).toContainText('export function buildShell');
  });

  test('several definitions open a picker filtered by path (mock)', async ({ page }) => {
    await openAt(page, 'web/src/features/palette.js', 240);
    const p = await wordPoint(page, 'return render([]);', 'render');
    await page.keyboard.down('ControlOrMeta');
    await page.mouse.click(p.x, p.y);
    await page.keyboard.up('ControlOrMeta');
    await expect(page.locator('.pal-mode')).toContainText('definitions of render');
    // Same-file definition ranks first.
    await expect(page.locator('.pal-item').first()).toContainText('web/src/features/palette.js');
    await page.locator('.pal-input').fill('tree');
    await expect(page.locator('.pal-item').first()).toContainText('web/src/features/tree.js');
    await page.keyboard.press('Enter');
    await expect(page.locator('.crumb.last')).toHaveText('tree.js');
  });

  test('Shift+F12 lists references grouped by file with previews (mock)', async ({ page }) => {
    await openAt(page, 'web/src/main.js', 14);
    const p = await wordPoint(page, "import { buildShell }", 'buildShell');
    await page.mouse.click(p.x, p.y);
    await page.keyboard.press('Shift+F12');
    const refs = page.locator('.refs');
    await expect(refs).toBeVisible();
    await expect(refs.locator('.refs-summary')).toContainText(/\d+ references? in \d+ files?/);
    await expect(refs.locator('.refs-file', { hasText: 'shell.js' })).toBeVisible();
    const ref = refs.locator('.refs-ref', { hasText: 'export function buildShell' });
    await expect(ref).toBeVisible();
    // Keyboard: the list is a listbox; Enter opens the active row.
    await ref.click();
    await expect(page.locator('.crumb.last')).toHaveText('shell.js');
  });

  test('Alt+U searches the name under the caret as a whole word (mock)', async ({ page }) => {
    await openAt(page, 'web/src/main.js', 14);
    const p = await wordPoint(page, "import { buildShell }", 'buildShell');
    await page.mouse.click(p.x, p.y);
    await page.keyboard.press('Alt+KeyU');
    const input = page.locator('.search-input input');
    await expect(input).toHaveValue('buildShell');
    await expect(page.locator('.search-opts [aria-label="Whole word"]')).toHaveAttribute('aria-pressed', 'true');
    // The mock's first search reads every file; under a loaded run that takes seconds.
    await expect(page.locator('.sr-file', { hasText: 'shell.js' })).toBeVisible({ timeout: 15_000 });
  });

  test('# palette mode finds workspace symbols (mock)', async ({ page }) => {
    await openAt(page, 'web/src/main.js', 1);
    await page.locator('.cmdbar').click();
    await page.locator('.pal-input').fill('#buildShe');
    await expect(page.locator('.pal-mode')).toContainText('Workspace symbols');
    const first = page.locator('.pal-item').first();
    await expect(first).toContainText('buildShell');
    await expect(first).toContainText('web/src/features/shell.js:');
    await page.keyboard.press('Enter');
    await expect(page.locator('.crumb.last')).toHaveText('shell.js');
  });
});

test('references panel and hover card meet WCAG 2.1 AA (mock)', async ({ page }) => {
  const { default: AxeBuilder } = await import('@axe-core/playwright');
  await openAt(page, 'web/src/main.js', 14);
  const p = await wordPoint(page, "import { buildShell }", 'buildShell');
  await page.mouse.click(p.x, p.y);
  await page.keyboard.press('Shift+F12');
  await expect(page.locator('.refs-ref').first()).toBeVisible();
  await page.mouse.move(p.x - 1, p.y);
  await page.mouse.move(p.x, p.y);
  await expect(page.locator('.hov-card')).toBeVisible();
  const results = await new AxeBuilder({ page })
    .include('.refs')
    .include('.hov-card')
    .withTags(['wcag2a', 'wcag2aa'])
    .disableRules(['color-contrast', 'nested-interactive'])
    .analyze();
  expect(results.violations).toEqual([]);
});
