import { test, expect } from '@playwright/test';

test.describe('Boot and Auth Screen', () => {
  test('boots into shell and home view in mock mode', async ({ page }) => {
    await page.goto('/web/index.html?mock=1');

    // Wait for boot completion (aria-busy removed from #app)
    const app = page.locator('#app');
    await expect(app).not.toHaveAttribute('aria-busy', 'true', { timeout: 10_000 });

    // Verify main shell components
    await expect(page.locator('.app')).toBeVisible();
    await expect(page.locator('.statusbar')).toBeVisible();
    await expect(page.locator('.home-inner')).toBeVisible();
    await expect(page.locator('.panel-head')).toBeVisible();

    // Verify boot performance marks
    const marks = await page.evaluate(() => {
      return {
        boot: performance.getEntriesByName('ferro:boot').length > 0,
        shell: performance.getEntriesByName('ferro:shell').length > 0,
        ready: performance.getEntriesByName('ferro:ready').length > 0,
      };
    });
    expect(marks.boot).toBe(true);
    expect(marks.shell).toBe(true);
    expect(marks.ready).toBe(true);
  });

  test('displays 401 auth screen when unauthorized', async ({ page }) => {
    await page.goto('/web/index.html?mock=auth');

    // Auth screen must render
    const authScreen = page.locator('.auth-screen');
    await expect(authScreen).toBeVisible({ timeout: 10_000 });

    const card = page.locator('.auth-card');
    await expect(card).toBeVisible();

    // Token input and submit button should be interactive
    const tokenInput = page.locator('input[aria-label="Session link or token"]');
    await expect(tokenInput).toBeVisible();
    const submitBtn = page.locator('.auth-card button[type="submit"]');
    await expect(submitBtn).toBeVisible();
  });
});
