import { test, expect } from '@playwright/test';

// Opening repositories (API.md § 7.2): the picker lists folders opened before, opens any folder
// by its path (errors stay in the dialog), and a pull request by its link.

test('open a recent repository, a folder by path, and a pull request (mock)', async ({ page }) => {
  await page.goto('/web/index.html?mock=1');
  await expect(page.locator('#app')).not.toHaveAttribute('aria-busy', 'true', { timeout: 10_000 });
  const name = page.locator('.ws-chip .ws-name');
  await expect(name).toHaveText('ferro');

  // The repository chip opens the picker: the other recent folders, then the two ways to open more.
  await page.locator('.ws-chip').click();
  const items = page.locator('.pal-item');
  await expect(items).toHaveText([/^api/, /^web-app/, /^Open Folder…/, /^Open Pull Request…/]);
  await items.filter({ hasText: '/Users/you/code/api' }).click();
  await expect(name).toHaveText('api');
  await expect(page.locator('.toast', { hasText: 'Opened api' })).toBeVisible();

  // Mod+Alt+O opens it too. A folder by its path: problems show in the dialog, which stays open.
  await page.keyboard.press('ControlOrMeta+Alt+KeyO');
  await expect(items.first()).toContainText('/Users/you/ferro');
  await items.filter({ hasText: 'Open Folder…' }).click();
  const dialog = page.getByRole('dialog', { name: 'Open folder' });
  const path = dialog.getByLabel('Folder path');
  await expect(path).toBeFocused();
  await path.fill('code/tools');
  await path.press('Enter');
  await expect(dialog.getByRole('alert')).toHaveText('path must be absolute, or start with ~/');
  await path.fill('~/code/tools');
  await dialog.getByRole('button', { name: 'Open' }).click();
  await expect(dialog).toBeHidden();
  await expect(name).toHaveText('tools');

  // A pull request by its link.
  await page.locator('.ws-chip').click();
  await items.filter({ hasText: 'Open Pull Request…' }).click();
  const link = page.getByRole('dialog', { name: 'Open pull request' }).getByLabel('Pull request link');
  await link.fill('github.com/owner/repo');
  await link.press('Enter');
  await expect(page.getByRole('dialog', { name: 'Open pull request' }).getByRole('alert')).toContainText('GitHub pull request');
  await link.fill('https://github.com/aniketshukla1/ferro/pull/42');
  await link.press('Enter');
  await expect(page.locator('.toast', { hasText: /Reviewing aniketshukla1\/ferro#42/ })).toBeVisible();
});
