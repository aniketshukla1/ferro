import { test, expect } from '@playwright/test';

// Checks → Every angle: one line per review angle, from the checks and the AI's last results for
// the change on screen; each line opens the detail or the AI mode that checks it.

const openChecks = (page) => page.evaluate(async () => (await import('/web/src/core/commands.js')).execute('checks.open'));
const row = (page, name) => page.locator('.ang-row', { has: page.locator('.ang-name', { hasText: new RegExp(`^${name}$`) }) });

test('every angle at a glance, filled in by the checks, the intent check and the AI review (mock)', async ({ page }) => {
  await page.goto('/web/index.html?mock=1');
  await expect(page.locator('#app')).not.toHaveAttribute('aria-busy', 'true', { timeout: 10_000 });
  await openChecks(page);
  await expect(page.locator('.ang-row .ang-name')).toHaveText(['Intent', 'Correctness', 'Tests', 'Security', 'Performance', 'Compatibility', 'Conventions', 'Coverage']);
  // From the checks themselves.
  await expect(row(page, 'Compatibility')).toHaveClass(/fail/);
  await expect(row(page, 'Compatibility').locator('.ang-text')).toContainText('API change');
  await expect(row(page, 'Intent').locator('.ang-text')).toHaveText('Not checked yet');

  // Intent: the line opens the Intent mode; its result comes back to the line.
  await row(page, 'Intent').getByRole('button', { name: 'Check intent' }).click();
  await expect(page.locator('.ai-seg [aria-pressed="true"]')).toHaveText('Intent');
  await page.getByLabel('What should this change do?').fill('Rank files whose name starts with the query first');
  await page.locator('.intent-bar').getByRole('button', { name: 'Check' }).click();
  await expect(page.locator('.intent-verdict')).toBeVisible();
  await openChecks(page);
  await expect(row(page, 'Intent').locator('.ang-text')).toHaveText('Not finished yet · 1 of 3 done');
  await expect(row(page, 'Intent')).toHaveClass(/warn/);

  // Correctness: the AI review's findings, counted by angle.
  await row(page, 'Correctness').getByRole('button', { name: 'Check correctness' }).click();
  await expect(page.locator('.ai-seg [aria-pressed="true"]')).toHaveText('Review');
  await page.getByRole('button', { name: /Review changes/ }).click();
  await page.locator('.dialog-foot').getByRole('button', { name: 'Review' }).click();
  await openChecks(page);
  await expect(row(page, 'Correctness').locator('.ang-text')).toHaveText(/AI finding/, { timeout: 10_000 });
  await expect(row(page, 'Performance').locator('.ang-text')).toHaveText(/AI finding/);
  await expect(row(page, 'Security').locator('.ang-text')).toContainText('AI finding');
});
