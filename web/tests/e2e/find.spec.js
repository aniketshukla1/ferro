import { test, expect } from '@playwright/test';

test.describe('Find in File', () => {
  test('finds matches past 512 KB in a large file', async ({ page }) => {
    // Open the 900 KB synthetic log file
    await page.goto('/web/index.html?mock=1&path=generated/large.log');
    await expect(page.locator('#app')).not.toHaveAttribute('aria-busy', 'true', { timeout: 15_000 });

    // Wait for the code view and rows to be rendered
    const rows = page.locator('.cv-row:not([hidden])');
    await expect(rows.first()).toBeVisible({ timeout: 10_000 });

    // Open find bar with Mod+F or via command
    await page.keyboard.press('ControlOrMeta+f');
    const findBar = page.locator('.findbar');
    if (!(await findBar.isVisible())) {
      await page.evaluate(() => {
        import('/web/src/core/commands.js').then((m) => m.execute('find.open'));
      });
    }
    const findInput = page.locator('.fb-input');
    await expect(findInput).toBeVisible();

    // Search for canary token placed at line 10 and line 11000 (offset > 512 KB)
    await findInput.fill('FERRO_CANARY_512KB');
    await findInput.press('Enter');

    const count = page.locator('.fb-count');
    await expect(count).toContainText('2', { timeout: 5_000 });

    // Step to the second match (line 11,000, past 512 KB)
    const nextBtn = page.locator('button[aria-label="Next match"]');
    await nextBtn.click();
    await page.waitForTimeout(300);

    // Verify view has scrolled to the match at line 11,000
    const renderedLines = await page.locator('.cv-row:not([hidden]) .cv-ln').allTextContents();
    const hasLineNear11k = renderedLines.some((ln) => {
      const n = Number(ln);
      return n >= 10_900 && n <= 11_100;
    });
    expect(hasLineNear11k).toBe(true);

    // Close find bar via close button or Escape
    const closeBtn = page.locator('button[aria-label="Close find"]');
    await closeBtn.click();
    await expect(findBar).not.toBeVisible();
  });
});
