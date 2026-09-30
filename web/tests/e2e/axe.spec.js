import { test, expect } from '@playwright/test';
import AxeBuilder from '@axe-core/playwright';

test.describe('Accessibility Audits (WCAG 2.1 AA)', () => {
  test('home screen meets WCAG 2.1 AA', async ({ page }) => {
    await page.goto('/web/index.html?mock=1');
    await expect(page.locator('#app')).not.toHaveAttribute('aria-busy', 'true', { timeout: 10_000 });
    await expect(page.locator('.home-inner')).toBeVisible();

    const results = await new AxeBuilder({ page })
      .withTags(['wcag2a', 'wcag2aa'])
      .disableRules(['color-contrast', 'nested-interactive'])
      .analyze();

    expect(results.violations).toEqual([]);
  });

  test('file viewer meets WCAG 2.1 AA', async ({ page }) => {
    await page.goto('/web/index.html?mock=1&path=README.md');
    await expect(page.locator('#app')).not.toHaveAttribute('aria-busy', 'true', { timeout: 10_000 });
    await expect(page.locator('.md-view')).toBeVisible();

    const results = await new AxeBuilder({ page })
      .withTags(['wcag2a', 'wcag2aa'])
      .disableRules(['color-contrast', 'nested-interactive'])
      .analyze();

    expect(results.violations).toEqual([]);
  });

  test('command palette meets WCAG 2.1 AA', async ({ page }) => {
    await page.goto('/web/index.html?mock=1');
    await expect(page.locator('#app')).not.toHaveAttribute('aria-busy', 'true', { timeout: 10_000 });

    await page.locator('.cmdbar').click();
    await expect(page.locator('.pal-input')).toBeVisible();
    await page.waitForTimeout(100);

    const results = await new AxeBuilder({ page })
      .withTags(['wcag2a', 'wcag2aa'])
      .disableRules(['color-contrast', 'nested-interactive'])
      .analyze();

    expect(results.violations).toEqual([]);
  });

  test('auth screen meets WCAG 2.1 AA', async ({ page }) => {
    await page.goto('/web/index.html?mock=auth');
    await expect(page.locator('.auth-screen')).toBeVisible({ timeout: 10_000 });

    const results = await new AxeBuilder({ page })
      .withTags(['wcag2a', 'wcag2aa'])
      .disableRules(['color-contrast', 'nested-interactive'])
      .analyze();

    expect(results.violations).toEqual([]);
  });

  test('changes panel meets WCAG 2.1 AA', async ({ page }) => {
    await page.goto('/web/index.html?mock=1');
    await expect(page.locator('#app')).not.toHaveAttribute('aria-busy', 'true', { timeout: 10_000 });
    await page.locator('.sw-btn[aria-label="Changes"]').click();
    await expect(page.locator('.git-panel-body')).toBeVisible();

    const results = await new AxeBuilder({ page })
      .withTags(['wcag2a', 'wcag2aa'])
      .disableRules(['color-contrast', 'nested-interactive'])
      .analyze();

    expect(results.violations).toEqual([]);
  });

  test('AI ask panel meets WCAG 2.1 AA', async ({ page }) => {
    await page.goto('/web/index.html?mock=1');
    await expect(page.locator('#app')).not.toHaveAttribute('aria-busy', 'true', { timeout: 10_000 });
    await page.locator('#insp-toggle').click();
    await expect(page.locator('.ai-seg')).toBeVisible({ timeout: 10_000 });

    const results = await new AxeBuilder({ page })
      .withTags(['wcag2a', 'wcag2aa'])
      .disableRules(['color-contrast', 'nested-interactive'])
      .analyze();

    expect(results.violations).toEqual([]);
  });

  test('AI review view meets WCAG 2.1 AA', async ({ page }) => {
    await page.goto('/web/index.html?mock=1');
    await expect(page.locator('#app')).not.toHaveAttribute('aria-busy', 'true', { timeout: 10_000 });
    await page.locator('#insp-toggle').click();
    await expect(page.locator('.ai-seg')).toBeVisible({ timeout: 10_000 });
    await page.locator('.ai-seg button[data-mode="review"]').click();
    await page.locator('.review-head button', { hasText: 'Review changes' }).click();
    await page.locator('.dialog .dialog-foot button', { hasText: 'Review' }).click();
    await expect(page.locator('.finding-card').first()).toBeVisible({ timeout: 5_000 });

    const results = await new AxeBuilder({ page })
      .withTags(['wcag2a', 'wcag2aa'])
      .disableRules(['color-contrast', 'nested-interactive'])
      .analyze();

    expect(results.violations).toEqual([]);
  });

  test('diff view meets WCAG 2.1 AA', async ({ page }) => {
    await page.goto('/web/index.html?mock=1');
    await expect(page.locator('#app')).not.toHaveAttribute('aria-busy', 'true', { timeout: 10_000 });
    await page.locator('.sw-btn[aria-label="Changes"]').click();
    await expect(page.locator('.git-panel-body')).toBeVisible();
    await page.locator('.git-file-row').first().click();
    await expect(page.locator('.diff-view')).toBeVisible();

    const results = await new AxeBuilder({ page })
      .withTags(['wcag2a', 'wcag2aa'])
      .disableRules(['color-contrast', 'nested-interactive'])
      .analyze();

    expect(results.violations).toEqual([]);
  });
});
