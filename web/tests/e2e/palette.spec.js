import { test, expect } from '@playwright/test';

test.describe('Command Palette and Modes', () => {
  test.beforeEach(async ({ page }) => {
    await page.goto('/web/index.html?mock=1');
    await expect(page.locator('#app')).not.toHaveAttribute('aria-busy', 'true', { timeout: 10_000 });
  });

  test('opens palette and switches prefix modes', async ({ page }) => {
    // Open palette with command bar click or Mod+K
    await page.locator('.cmdbar').click();
    const input = page.locator('.pal-input');
    await expect(input).toBeVisible();

    // Mode: Commands (>)
    await input.fill('>');
    await expect(page.locator('.pal-mode')).toContainText('Commands');
    const cmdItems = page.locator('.pal-item');
    await expect(cmdItems.first()).toBeVisible();

    // Mode: Line (:)
    await input.fill(':');
    await expect(page.locator('.pal-mode')).toContainText('Go to line');

    // Mode: Help (?)
    await input.fill('?');
    await expect(page.locator('.pal-mode')).toContainText('Help');
    await expect(page.locator('.pal-item').first()).toBeVisible();

    // Escape closes palette
    await page.keyboard.press('Escape');
    await expect(input).not.toBeVisible();
  });

  test('detects GitHub PR URL and suggests PR mode', async ({ page }) => {
    await page.locator('.cmdbar').click();
    const input = page.locator('.pal-input');
    await expect(input).toBeVisible();

    await input.fill('https://github.com/aniketshukla1/ferro/pull/42');
    await page.waitForTimeout(100);

    const firstItem = page.locator('.pal-item').first();
    await expect(firstItem).toBeVisible();
    const text = await firstItem.textContent();
    expect(text).toContain('pull/42');
    expect(text?.toLowerCase()).toContain('review in ferro');
    await page.keyboard.press('Escape');
  });

  test('handles stale-response race condition without overwriting latest query', async ({ page }) => {
    await page.locator('.cmdbar').click();
    const input = page.locator('.pal-input');
    await expect(input).toBeVisible();

    // Fire query 1 then immediately query 2
    await input.fill('server');
    await page.waitForTimeout(5);
    await input.fill('readme');

    // Wait for network/mock to settle
    await page.waitForTimeout(300);

    // Results must correspond to 'readme', not 'server'
    const firstItem = page.locator('.pal-item').first();
    await expect(firstItem).toBeVisible();
    const text = await firstItem.textContent();
    expect(text?.toLowerCase()).toContain('readme');
  });

  test('file search opens file in editor', async ({ page }) => {
    await page.locator('.cmdbar').click();
    const input = page.locator('.pal-input');
    await expect(input).toBeVisible();

    await input.fill('README.md');
    await page.waitForTimeout(150);

    const item = page.locator('.pal-item').first();
    await expect(item).toBeVisible();
    await item.click();

    // Tab strip should now contain README.md
    await expect(page.locator('.tab-name', { hasText: 'README.md' })).toBeVisible();
  });
});
