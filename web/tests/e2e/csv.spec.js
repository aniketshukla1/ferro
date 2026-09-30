import { test, expect } from '@playwright/test';

// CSV/TSV table view: parsed table, sticky header, Table/Source toggle.
test('a CSV opens as a table with quoted fields intact; Alt+M shows the source (mock)', async ({ page }) => {
  await page.goto('/web/index.html?mock=1&path=web/tests/fixtures/bench.csv');
  await expect(page.locator('#app')).not.toHaveAttribute('aria-busy', 'true', { timeout: 10_000 });
  const grid = page.locator('.csv-grid');
  await expect(grid).toBeVisible();
  await expect(page.locator('.csv-head .csv-th')).toHaveText(['repo', 'files', 'index_ms', 'note']);
  await expect(page.locator('.csv-note')).toHaveText('3 rows × 4 columns · comma');
  const k8s = grid.locator('.csv-row', { hasText: 'kubernetes' });
  await expect(k8s.locator('.csv-td').nth(3)).toHaveText('large, Go');
  await expect(grid.locator('.csv-row', { hasText: 'typescript' }).locator('.csv-td').nth(3)).toContainText('multi');
  await page.keyboard.press('Alt+KeyM');
  await expect(grid).toBeHidden();
  await expect(page.locator('.cv-row', { hasText: 'kubernetes,31378' })).toBeVisible();
  await page.locator('.csv-bar button', { hasText: 'Table' }).click();
  await expect(grid).toBeVisible();
});
