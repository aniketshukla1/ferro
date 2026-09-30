import { test, expect } from '@playwright/test';

test.describe('Search Panel', () => {
  test('executes search, handles streaming results and abort/cancellation', async ({ page }) => {
    await page.goto('/web/index.html?mock=1');
    await expect(page.locator('#app')).not.toHaveAttribute('aria-busy', 'true', { timeout: 10_000 });

    // Open search panel by clicking the Search sidebar tab
    await page.locator('.sw-btn[aria-label="Search"]').click();
    const searchInput = page.locator('input[aria-label="Search in files"]');
    await expect(searchInput).toBeVisible();

    // Type query
    await searchInput.fill('ferro');
    const summary = page.locator('.search-summary');
    // The mock's first search reads ~200 files from disk one by one: 4-7 s cold on WebKit.
    await expect(summary).toContainText(/result|file/i, { timeout: 15_000 });

    // Results container should render groups and hits
    const groups = page.locator('.sr-group');
    await expect(groups.first()).toBeVisible();

    // Verify abort on rapid query changes / cancellation
    await searchInput.fill('crates');
    await searchInput.fill('');
    await page.waitForTimeout(200);

    // Cleared query should clear results and summary
    await expect(page.locator('.sr-group')).toHaveCount(0);
    await expect(summary).toHaveText('');
  });

  test('search options (case, word, regex) toggle properly', async ({ page }) => {
    await page.goto('/web/index.html?mock=1');
    await expect(page.locator('#app')).not.toHaveAttribute('aria-busy', 'true', { timeout: 10_000 });

    // Open search panel
    await page.locator('.sw-btn[aria-label="Search"]').click();
    const caseBtn = page.locator('button[aria-label="Match case"]');
    const wordBtn = page.locator('button[aria-label="Whole word"]');
    const regexBtn = page.locator('button[aria-label="Regular expression"]');

    await expect(caseBtn).toBeVisible();
    await caseBtn.click();
    await expect(caseBtn).toHaveAttribute('aria-pressed', 'true');

    await wordBtn.click();
    await expect(wordBtn).toHaveAttribute('aria-pressed', 'true');

    await regexBtn.click();
    await expect(regexBtn).toHaveAttribute('aria-pressed', 'true');
  });
});
