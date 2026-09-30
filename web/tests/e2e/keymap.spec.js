import { test, expect } from '@playwright/test';

test.describe('Keyboard Map (macOS and Linux layouts)', () => {
  test.beforeEach(async ({ page }) => {
    await page.goto('/web/index.html?mock=1');
    await expect(page.locator('#app')).not.toHaveAttribute('aria-busy', 'true', { timeout: 10_000 });
  });

  test('toggles sidebar and inspector via buttons and layout state', async ({ page }) => {
    const app = page.locator('.app');
    const sbToggle = page.locator('#sb-toggle');
    const inspToggle = page.locator('#insp-toggle');

    await expect(sbToggle).toHaveAttribute('aria-pressed', 'true');
    await expect(app).not.toHaveClass(/no-sidebar/);

    // Toggle sidebar closed
    await sbToggle.click();
    await expect(sbToggle).toHaveAttribute('aria-pressed', 'false');
    await expect(app).toHaveClass(/no-sidebar/);

    // Toggle sidebar open
    await sbToggle.click();
    await expect(sbToggle).toHaveAttribute('aria-pressed', 'true');
    await expect(app).not.toHaveClass(/no-sidebar/);

    // Toggle inspector open
    await expect(inspToggle).toHaveAttribute('aria-pressed', 'false');
    await inspToggle.click();
    await expect(inspToggle).toHaveAttribute('aria-pressed', 'true');
    await expect(app).toHaveClass(/force-inspector/);

    // Toggle inspector closed
    await inspToggle.click();
    await expect(inspToggle).toHaveAttribute('aria-pressed', 'false');
  });

  test('opens settings dialog and closes via Escape', async ({ page }) => {
    // Open settings via topbar button or shortcut
    await page.locator('button[aria-label="Settings"]').click();
    const settingsDialog = page.locator('.dialog.settings-dialog');
    await expect(settingsDialog).toBeVisible();

    await page.keyboard.press('Escape');
    await expect(settingsDialog).not.toBeVisible();
  });

  test('opens shortcuts sheet via ? outside inputs', async ({ page }) => {
    await page.keyboard.press('?');
    const keysSheet = page.locator('.keys-sheet');
    await expect(keysSheet).toBeVisible();

    await page.keyboard.press('Escape');
    await expect(keysSheet).not.toBeVisible();
  });

  test('toggles light and dark theme', async ({ page }) => {
    const themeInitial = await page.evaluate(() => document.documentElement.dataset.theme);
    const themeBtn = page.locator('button[aria-label="Toggle light and dark"]');
    await themeBtn.click();
    const themeAfter = await page.evaluate(() => document.documentElement.dataset.theme);
    expect(themeAfter).not.toBe(themeInitial);
  });
});
