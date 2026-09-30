import { test, expect } from '@playwright/test';

// Links from `ferro open` and the editor extensions: ?path=&line=&view= open the file at the
// line and then the view, and leave a clean address behind.

test('a link opens the file at its line, then the view', async ({ page }) => {
  await page.goto('/web/index.html?mock=1&path=crates/ferro-core/src/lib.rs&line=3&view=history');
  await expect(page.locator('#app')).not.toHaveAttribute('aria-busy', 'true', { timeout: 10_000 });
  await expect(page.locator('.tab.active, [role="tab"][aria-selected="true"]', { hasText: 'lib.rs' }).first()).toBeVisible();
  await expect(page.locator('.cv-row.cur')).toHaveAttribute('data-n', '3');
  await expect(page.locator('.hi-row').first()).toBeVisible();
  expect(new URL(page.url()).searchParams.has('path')).toBe(false);
  expect(new URL(page.url()).searchParams.has('view')).toBe(false);
});

test('view=checks opens the Checks tab', async ({ page }) => {
  await page.goto('/web/index.html?mock=1&view=checks');
  await expect(page.locator('#app')).not.toHaveAttribute('aria-busy', 'true', { timeout: 10_000 });
  await expect(page.locator('.ck-sec[data-check="breaking"]')).toBeVisible();
});
