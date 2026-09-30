import { test, expect } from '@playwright/test';

// AI change notes (API.md § 10.8) on the mock: Explain tags every hunk and summarizes each file.

test('Explain adds a summary per file and a tag per hunk (mock)', async ({ page }) => {
  await page.goto('/web/index.html?mock=1');
  await expect(page.locator('#app')).not.toHaveAttribute('aria-busy', 'true', { timeout: 10_000 });
  await page.getByRole('tab', { name: 'History' }).click();
  await page.locator('.hi-row', { hasText: 'initial tree' }).click();
  await expect(page.locator('.hc-bar')).toContainText('feat(core): initial tree');
  await expect(page.locator('.diff-hunk-sep').first()).toBeVisible();

  await page.getByRole('button', { name: 'Explain', exact: true }).click();
  await expect(page.locator('.toast', { hasText: /Explained \d+ files?/ })).toBeVisible();
  const fileNote = page.locator('.diff-note.file-note:visible').first();
  await expect(fileNote).toContainText('clearer names');
  const hunkNote = page.locator('.diff-note.hunk-note:visible').first();
  await expect(hunkNote.locator('.diff-note-kind')).toHaveText(/Added|Removed|Changed/);
  await expect(hunkNote).toHaveAttribute('title', /empty-input|Renames/);

  // Another comparison drops the notes (they belong to one base/target pair).
  await page.locator('.hi-row', { hasText: 'setup pipeline' }).click();
  await expect(page.locator('.hc-bar')).toContainText('chore: setup pipeline');
  await expect(page.locator('.diff-note:visible')).toHaveCount(0);
});
