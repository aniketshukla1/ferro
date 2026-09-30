import { test, expect } from '@playwright/test';

test.describe('Huge File Viewer (400k lines)', () => {
  test('opens 400k-line file with virtualized rendering and scaled scrolling', async ({ page }) => {
    await page.goto('/web/index.html?mock=1&path=generated/huge.log');
    await expect(page.locator('#app')).not.toHaveAttribute('aria-busy', 'true', { timeout: 15_000 });

    // Verify code view is active
    const cv = page.locator('.cv');
    await expect(cv).toBeVisible({ timeout: 10_000 });

    // Verify virtual rendering: only viewport rows exist in DOM
    const visibleRows = page.locator('.cv-row:not([hidden])');
    await expect(visibleRows.first()).toBeVisible({ timeout: 10_000 });
    const rowCount = await visibleRows.count();
    expect(rowCount).toBeGreaterThan(0);
    expect(rowCount).toBeLessThan(150);

    // Initial lines start at line 1
    const firstLineNum = await page.locator('.cv-row .cv-ln').first().textContent();
    expect(Number(firstLineNum)).toBe(1);

    // Fling scroll down deep into the file
    await cv.evaluate((el) => {
      el.scrollTop = el.scrollHeight / 2;
      el.dispatchEvent(new Event('scroll'));
    });

    // Wait for the window to settle at mid-point
    await page.waitForTimeout(300);

    // Check that rendered line numbers jumped deep into the file (near ~200k)
    const midLineNum = await page.locator('.cv-row .cv-ln').first().textContent();
    const midNum = Number(midLineNum);
    expect(midNum).toBeGreaterThan(50_000);
    expect(midNum).toBeLessThan(350_000);

    // Fling scroll to the very bottom
    await cv.evaluate((el) => {
      el.scrollTop = el.scrollHeight;
      el.dispatchEvent(new Event('scroll'));
    });
    await page.waitForTimeout(300);

    // Last rendered lines should reach near 400,000
    const lastLineNum = await page.locator('.cv-row .cv-ln').last().textContent();
    expect(Number(lastLineNum)).toBeGreaterThan(390_000);
  });
});
