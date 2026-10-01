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

test('Explain gives each change a verdict; a suggestion opens as a diff to copy (mock)', async ({ page }) => {
  await page.goto('/web/index.html?mock=1');
  await expect(page.locator('#app')).not.toHaveAttribute('aria-busy', 'true', { timeout: 10_000 });
  await page.getByRole('tab', { name: 'History' }).click();
  await page.locator('.hi-ref-sel').selectOption('feature/search');
  await page.locator('.hi-row', { hasText: 'fuzzy scoring by path depth' }).click();
  await expect(page.locator('.hc-bar')).toContainText('fuzzy scoring by path depth');
  await page.getByRole('button', { name: 'Explain', exact: true }).click();
  await expect(page.locator('.toast', { hasText: /Explained \d+ files?/ })).toContainText('1 change could be better');

  const flagged = page.locator('.diff-note.hunk-note.has-more:visible').first();
  await expect(flagged.locator('.diff-note-verdict')).toHaveText('Could be better');
  await expect(flagged).toContainText('Clamping at 0 makes every deep match tie');
  await expect(page.locator('.diff-note.hunk-note:visible:not(.has-more) .diff-note-verdict:visible')).toHaveCount(0);
  await flagged.getByRole('button', { name: 'Could be better' }).press('Enter');
  const dialog = page.getByRole('dialog', { name: 'Could be better' });
  await expect(dialog.locator('.hunk-review-diff .hr-del', { hasText: 'Some(s.max(0))' })).toBeVisible();
  await expect(dialog.locator('.hunk-review-diff .hr-add', { hasText: 'Some(s - depth * opts.depth_penalty)' })).toBeVisible();
  await expect(dialog.getByRole('button', { name: 'Copy code' })).toBeVisible();
  // A commit is not the working tree: the suggestion can be copied, not applied.
  await expect(dialog.getByRole('button', { name: 'Apply' })).toHaveCount(0);
  await dialog.locator('.dialog-foot').getByRole('button', { name: 'Close' }).click();
  await expect(dialog).toBeHidden();
});
