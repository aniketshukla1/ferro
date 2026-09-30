import { test, expect } from '@playwright/test';

// Background updates (API.md § 13) against the mock: check → install → restart → reload.

test('check, install and restart into an update (mock)', async ({ page }) => {
  await page.goto('/web/index.html?mock=1');
  await expect(page.locator('#app')).not.toHaveAttribute('aria-busy', 'true', { timeout: 10_000 });

  await page.locator('.cmdbar').click();
  await page.locator('.pal-input').fill('>Check for Updates');
  await expect(page.locator('.pal-item').first()).toContainText('Check for Updates');
  await page.keyboard.press('Enter');
  await expect(page.locator('.toast', { hasText: 'ferro 0.3.0 is available' })).toBeVisible();

  const item = page.locator('.statusbar .sb-group.right .sb-item').first();
  await expect(item).toHaveText('Update 0.3.0');
  await item.click();
  await expect(item).toHaveText('Restart to update');

  await item.click();
  const dialog = page.getByRole('dialog');
  await expect(dialog).toContainText('Restart to run ferro 0.3.0?');
  const reloaded = page.waitForEvent('load');
  await dialog.getByRole('button', { name: 'Restart' }).click();
  await reloaded;
  await expect(page.locator('#app')).not.toHaveAttribute('aria-busy', 'true', { timeout: 10_000 });
  await expect(page.locator('.statusbar .sb-item', { hasText: /Update|Restart to update/ })).toHaveCount(0);
});
