import { test, expect } from '@playwright/test';

test.describe('File Tree', () => {
  test('renders tree with virtual scrolling on 60k files', async ({ page }) => {
    await page.goto('/web/index.html?mock=big');
    await expect(page.locator('#app')).not.toHaveAttribute('aria-busy', 'true', { timeout: 15_000 });

    const tree = page.locator('.tree');
    await expect(tree).toBeVisible();

    // Wait for tree rows to load and mount
    const visibleRows = page.locator('.tree-row:not([hidden])');
    await expect(visibleRows.first()).toBeVisible({ timeout: 10_000 });

    // Verify virtual rendering: only a viewport slice of rows are mounted in DOM
    const rowCount = await visibleRows.count();
    expect(rowCount).toBeGreaterThan(0);
    expect(rowCount).toBeLessThan(150); // far below 60,000

    // Expand a folder: click the first folder row
    const firstFolder = page.locator('.tree-row.dir:not([hidden])').first();
    if (await firstFolder.count() > 0) {
      const initialExpanded = await firstFolder.getAttribute('aria-expanded');
      await firstFolder.click();
      await expect(firstFolder).toHaveAttribute('aria-expanded', initialExpanded === 'true' ? 'false' : 'true');
    }
  });

  test('tree filter box filters entries and restores on clear', async ({ page }) => {
    await page.goto('/web/index.html?mock=1');
    await expect(page.locator('#app')).not.toHaveAttribute('aria-busy', 'true', { timeout: 10_000 });

    // Wait for initial tree rows to load
    const visibleRows = page.locator('.tree-row:not([hidden])');
    await expect(visibleRows.first()).toBeVisible({ timeout: 10_000 });

    const filterInput = page.locator('input.tree-filter');
    await expect(filterInput).toBeVisible();

    // Type filter
    await filterInput.fill('readme');
    await page.waitForTimeout(200);

    const filteredCount = await page.locator('.tree-row:not([hidden])').count();
    expect(filteredCount).toBeGreaterThan(0);
    expect(filteredCount).toBeLessThan(10); // only readme and its directory ancestors

    const text = await page.locator('.tree-row:not([hidden])').last().textContent();
    expect(text?.toLowerCase()).toContain('readme');

    // Clear filter
    await filterInput.fill('');
    await page.waitForTimeout(200);
    const restoredCount = await page.locator('.tree-row:not([hidden])').count();
    expect(restoredCount).toBeGreaterThan(filteredCount);
  });

  test('toolbar buttons: expand all, collapse all, toggle ignored', async ({ page }) => {
    await page.goto('/web/index.html?mock=1');
    await expect(page.locator('#app')).not.toHaveAttribute('aria-busy', 'true', { timeout: 10_000 });
    await expect(page.locator('.tree-row:not([hidden])').first()).toBeVisible({ timeout: 10_000 });

    // One button: Expand all while every folder is closed, Collapse all once any is open.
    const foldBtn = page.locator('.tree-fold-btn');
    const ignoredBtn = page.locator('button[aria-label="Toggle ignored files"]');
    await expect(foldBtn).toBeVisible();
    await expect(ignoredBtn).toBeVisible();
    if ((await foldBtn.getAttribute('aria-label')) === 'Collapse all') await foldBtn.click();
    await expect(foldBtn).toHaveAttribute('aria-label', 'Expand all');

    await foldBtn.click();
    await expect(foldBtn).toHaveAttribute('aria-label', 'Collapse all');
    await expect.poll(() => page.locator('.tree-row:not([hidden])[aria-expanded="true"]').count()).toBeGreaterThan(0);

    await foldBtn.click();
    await expect(foldBtn).toHaveAttribute('aria-label', 'Expand all');
    await expect(page.locator('.tree-row:not([hidden])[aria-expanded="true"]')).toHaveCount(0);

    // Opening one folder by hand turns it back into Collapse all.
    await page.locator('.tree-row.dir').first().click();
    await expect(foldBtn).toHaveAttribute('aria-label', 'Collapse all');

    // Toggle ignored files
    await ignoredBtn.click();
    await expect(ignoredBtn).toHaveClass(/active/);
    await expect(ignoredBtn).toHaveAttribute('aria-pressed', 'true');
    await ignoredBtn.click();
    await expect(ignoredBtn).not.toHaveClass(/active/);
    await expect(ignoredBtn).toHaveAttribute('aria-pressed', 'false');
  });
});

test('deleted files stay in the tree, struck through, and open their diff (mock)', async ({ page }) => {
  await page.goto('/web/index.html?mock=1');
  await expect(page.locator('#app')).not.toHaveAttribute('aria-busy', 'true', { timeout: 10_000 });
  await page.evaluate(() => { localStorage.setItem('ferro.tips.dismissed', 'true'); });
  const tree = page.locator('[role="tree"]');
  for (const dir of ['crates', 'ferro-core', 'src']) {
    const row = tree.locator('.tree-row.dir', { hasText: dir }).last();
    if ((await row.getAttribute('aria-expanded')) !== 'true') await row.click();
  }
  const gone = tree.locator('.tree-row.git-D', { hasText: 'retired.rs' });
  await expect(gone).toBeVisible();
  await expect(gone.locator('.gitc')).toHaveText('D');
  await gone.click();
  await expect(page.locator('.diff-view')).toBeVisible();
  await expect(page.locator('.diff-file-path', { hasText: 'retired.rs' })).toBeVisible();
});
