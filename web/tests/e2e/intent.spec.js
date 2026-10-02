import { test, expect } from '@playwright/test';

// Intent check (AI tab → Intent): describe what the change should do, then each requirement
// comes back done, partly done or missing, with links to the lines that show it.

test('intent check: requirements with their status, links into the code, and the draft kept (mock)', async ({ page }) => {
  await page.goto('/web/index.html?mock=1');
  await expect(page.locator('#app')).not.toHaveAttribute('aria-busy', 'true', { timeout: 10_000 });
  await page.locator('.topbar').getByRole('button', { name: 'Ask AI' }).click();
  await page.locator('.ai-seg').getByRole('button', { name: 'Intent' }).click();
  const ask = page.getByLabel('What should this change do?');
  await expect(ask).toBeFocused();

  // Nothing described yet: say so instead of running.
  await page.locator('.intent-bar').getByRole('button', { name: 'Check' }).click();
  await expect(page.locator('.intent [role="status"]')).toHaveText('Describe what the change should do first.');

  await ask.fill('Rank files whose name starts with the query first, penalize deep paths, and keep the old ranking behind a setting.');
  await ask.press('ControlOrMeta+Enter');
  const verdict = page.locator('.intent-verdict');
  await expect(verdict).toContainText('Not finished yet');
  await expect(verdict).toContainText('1 of 3 done');
  await expect(page.locator('.intent-req .intent-status')).toHaveText(['Done', 'Partly', 'Missing']);
  await expect(page.locator('.intent-label')).toHaveText(['Changes your description does not mention', 'Edge cases not handled', 'Tests to add']);

  // A requirement's evidence opens the file at that line.
  await page.getByRole('button', { name: 'crates/ferro-core/src/fuzzy.rs:118' }).click();
  await expect(page.locator('.crumb.last')).toHaveText('fuzzy.rs');

  await page.locator('.intent-foot').getByRole('button', { name: 'Copy as Markdown' }).click();
  await expect(page.locator('.toast', { hasText: 'Copied as Markdown' })).toBeVisible();

  // The description is kept for the next check.
  await page.reload();
  await expect(page.locator('#app')).not.toHaveAttribute('aria-busy', 'true', { timeout: 10_000 });
  await page.locator('.topbar').getByRole('button', { name: 'Ask AI' }).click();
  await page.locator('.ai-seg').getByRole('button', { name: 'Intent' }).click();
  await expect(page.getByLabel('What should this change do?')).toHaveValue(/^Rank files whose name starts with the query first/);
});
