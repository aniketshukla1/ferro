import { test, expect } from '@playwright/test';

// Inline edit (API.md § 4.10, § 10.9) against the mock: type in place or ask the AI, then save.
const FILE = 'crates/ferro-core/src/lib.rs';

async function openFile(page) {
  await page.goto(`/web/index.html?mock=1&path=${FILE}&line=1`);
  await expect(page.locator('#app')).not.toHaveAttribute('aria-busy', 'true', { timeout: 10_000 });
  await expect(page.locator('.cv-row[data-n="2"] .cv-code')).not.toBeEmpty();
}
const lineText = (page, n) => page.locator(`.cv-row[data-n="${n}"] .cv-code`).innerText();

test('select a line, edit it in place, save, and undo from the toast', async ({ page }) => {
  await openFile(page);
  const before = await lineText(page, 2);
  await page.locator('.cv-row[data-n="2"] .cv-ln').click();
  const bar = page.locator('.cv-selbar');
  await expect(bar).toBeVisible();
  await bar.getByRole('button', { name: 'Edit', exact: true }).click();

  const box = page.locator('.cv-zone .ie');
  const input = box.locator('.ie-input');
  await expect(input).toBeFocused();
  await expect(input).toHaveValue(before);
  await expect(page.locator('.cv-row[data-n="2"]')).toHaveClass(/editing/);
  await input.fill('pub mod renamed;\npub mod added;');
  await expect(box.locator('.ie-hl')).toContainText('pub mod added;');
  await page.keyboard.press('ControlOrMeta+s');

  await expect(box).toHaveCount(0);
  await expect(page.locator('.toast', { hasText: 'Saved lib.rs' })).toBeVisible();
  await expect(page.locator('.cv-row[data-n="2"] .cv-code')).toHaveText('pub mod renamed;');
  await expect(page.locator('.cv-row[data-n="3"] .cv-code')).toHaveText('pub mod added;');

  await page.locator('.toast', { hasText: 'Saved lib.rs' }).getByRole('button', { name: 'Undo' }).click();
  await expect(page.locator('.toast', { hasText: 'Edit undone' })).toBeVisible();
  await expect(page.locator('.cv-row[data-n="2"] .cv-code')).toHaveText(before);
});

test('Alt+K asks the AI; its proposal shows as a diff and saves like a typed edit', async ({ page }) => {
  await openFile(page);
  const before = await lineText(page, 1);
  const name = before.match(/pub mod (\w+);/)[1];
  await page.locator('.cv-row[data-n="1"] .cv-ln').click();
  await page.keyboard.press('Alt+KeyK');
  const box = page.locator('.cv-zone .ie');
  const prompt = box.locator('.ie-prompt');
  await expect(prompt).toBeFocused();
  await prompt.fill(`rename ${name} to renamed_by_ai`);
  await page.keyboard.press('Enter');

  await expect(box.locator('.ie-input')).toHaveValue(before.replace(name, 'renamed_by_ai'));
  const diff = box.locator('.ie-diff');
  await expect(diff).toBeVisible();
  await expect(diff.locator('.ie-drow.del')).toHaveText(new RegExp(name));
  await expect(diff.locator('.ie-drow.add')).toContainText('renamed_by_ai');
  await expect(diff.locator('.ie-diff-head')).toHaveText('+1 −1');

  // Undo AI change puts the typed text back; asking again and saving writes the file.
  await box.getByRole('button', { name: 'Undo AI change' }).click();
  await expect(box.locator('.ie-input')).toHaveValue(before);
  await prompt.fill(`rename ${name} to renamed_by_ai`);
  await page.keyboard.press('Enter');
  await expect(box.locator('.ie-input')).toHaveValue(before.replace(name, 'renamed_by_ai'));
  await box.getByRole('button', { name: 'Save' }).click();
  await expect(page.locator('.cv-row[data-n="1"] .cv-code')).toHaveText(before.replace(name, 'renamed_by_ai'));
});

test('a line that changed on disk is refused, and Esc twice discards', async ({ page }) => {
  await openFile(page);
  await page.locator('.cv-row[data-n="2"] .cv-ln').click();
  await page.keyboard.press('Alt+KeyI');
  const box = page.locator('.cv-zone .ie');
  const input = box.locator('.ie-input');
  await expect(input).toBeFocused();
  await input.fill('pub mod mine;');
  // Someone else changes line 2 meanwhile.
  await page.evaluate((path) => { const d = window.__ferroMock.repo.docs.get(path); d.lines[1] = 'pub mod theirs;'; }, FILE);
  await box.getByRole('button', { name: 'Save' }).click();
  await expect(box.locator('.ie-err')).toContainText('changed on disk');
  await box.getByRole('button', { name: 'Compare with the file now' }).click();
  await expect(box.locator('.ie-drow.del')).toHaveText(/pub mod theirs;/);

  await input.focus();
  await page.keyboard.press('Escape');
  await expect(box.locator('.ie-err')).toContainText('Esc again');
  await page.keyboard.press('Escape');
  await expect(box).toHaveCount(0);
  await expect(page.locator('.cv-row.editing')).toHaveCount(0);
});

test('read-only servers offer no inline edit', async ({ page }) => {
  await openFile(page);
  await page.evaluate(async () => { const { store } = await import('/web/src/core/store.js'); store.set('meta', { ...store.get('meta'), readOnly: true }); });
  await page.locator('.cv-row[data-n="2"] .cv-ln').click();
  await expect(page.locator('.cv-selbar')).toHaveCount(0);
  await page.keyboard.press('Alt+KeyI');
  await expect(page.locator('.cv-zone')).toHaveCount(0);
});
