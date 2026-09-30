import { test, expect } from '@playwright/test';

// Team review memory (API.md § 17) on the mock: ignore rules from findings, team vs personal,
// suggestions, conventions.

async function palette(page, text) {
  await page.locator('.cmdbar').click();
  await page.locator('.pal-input').fill(`>${text}`);
  await expect(page.locator('.pal-item').first()).toContainText(text);
  await page.keyboard.press('Enter');
}

test('ignoring a security finding creates a team rule that hides it', async ({ page }) => {
  await page.goto('/web/index.html?mock=1');
  await expect(page.locator('#app')).not.toHaveAttribute('aria-busy', 'true', { timeout: 10_000 });
  await palette(page, 'Check This Change');
  const sec = page.locator('.ck-sec[data-check="security"]');
  await expect(sec.locator('.ck-status')).toHaveText('2 findings · 1 serious');
  await sec.locator('.ck-item', { hasText: 'GitHub token' }).getByRole('button', { name: 'Ignore…' }).click();
  const dialog = page.getByRole('dialog', { name: 'Don’t report this again' });
  await expect(dialog.getByLabel('This rule (secret.github-token)')).toBeChecked();
  await dialog.getByLabel('Everywhere').check();
  await dialog.getByLabel('Reason').fill('rotated; kept as a revoked sample');
  await dialog.getByLabel(/My team/).check();
  await dialog.getByRole('button', { name: 'Save rule' }).click();
  await expect(sec.locator('.ck-status')).toHaveText('1 finding · 1 ignored');
  await sec.getByRole('button', { name: 'Show 1 ignored finding' }).click();
  await expect(sec.locator('.ck-hidden .ck-item')).toContainText('ignored by team');
  await expect(sec.locator('.ck-hidden .ck-item')).toContainText('Ignored: rotated; kept as a revoked sample');

  await palette(page, 'Team Review Memory');
  const mem = page.locator('.mem');
  const team = mem.locator('.mem-group').first();
  await expect(team.locator('.mem-head')).toHaveText('Team · 1');
  await expect(team.locator('.mem-desc')).toHaveText('Security: don’t report secret.github-token anywhere');
  await team.getByRole('button', { name: 'Make personal' }).click();
  await expect(team.locator('.mem-head')).toHaveText('Team · 0');
  const mine = mem.locator('.mem-group').nth(1);
  await expect(mine.locator('.mem-head')).toHaveText('Just me · 1');
  await mine.getByRole('button', { name: 'Delete' }).click();
  await page.getByRole('dialog', { name: 'Delete this rule?' }).getByRole('button', { name: 'Delete' }).click();
  await expect(mine.locator('.mem-head')).toHaveText('Just me · 0');
});

test('suggestions come from repeated dismissals; conventions are added by hand', async ({ page }) => {
  await page.goto('/web/index.html?mock=1');
  await expect(page.locator('#app')).not.toHaveAttribute('aria-busy', 'true', { timeout: 10_000 });
  await palette(page, 'Team Review Memory');
  const mem = page.locator('.mem');
  const sug = mem.locator('.mem-sug');
  await expect(sug).toContainText('AI review: don’t report “Magic number” (style) in crates/ferro-core/src/**');
  await expect(sug).toContainText('Dismissed 2 times');
  await sug.getByRole('button', { name: 'Not now' }).click();
  await expect(mem.locator('.mem-sug')).toHaveCount(0);

  await palette(page, 'Add Team Convention…');
  const dialog = page.getByRole('dialog', { name: 'Add a team convention' });
  await dialog.getByLabel('Convention').fill('Every public function has a doc comment');
  await dialog.getByLabel(/My team/).check();
  await dialog.getByRole('button', { name: 'Save rule' }).click();
  await expect(mem.locator('.mem-group').first().locator('.mem-desc')).toHaveText('Every public function has a doc comment');
});

test('learn from merged pull requests proposes conventions with the comments behind them', async ({ page }) => {
  await page.goto('/web/index.html?mock=1');
  await expect(page.locator('#app')).not.toHaveAttribute('aria-busy', 'true', { timeout: 10_000 });
  await palette(page, 'Team Review Memory');
  const tab = page.locator('.mem');
  await tab.getByRole('button', { name: 'Learn from PRs' }).click();
  const dialog = page.getByRole('dialog', { name: 'Learn from merged pull requests' });
  await expect(dialog).toContainText('github.com/aniketshukla1/ferro');
  await expect(dialog).toContainText('Only comment text is sent, not code');
  await dialog.getByLabel('How many pull requests').selectOption('20');
  await dialog.getByRole('button', { name: 'Learn' }).click();
  await expect(page.locator('.toast', { hasText: 'Found 2 conventions' })).toBeVisible();
  await expect(tab.locator('.mem-learn')).toContainText('Learned from merged pull requests');

  const sug = tab.locator('.mem-sug', { hasText: 'Add a regression test with every bug fix' });
  await expect(sug).toContainText('Asked for in 3 review comments across 3 merged pull requests');
  const link = sug.locator('.mem-evidence a').first();
  await expect(link).toHaveText('#412');
  await expect(link).toHaveAttribute('href', 'https://github.com/aniketshukla1/ferro/pull/412');
  await expect(link).toHaveAttribute('rel', 'noopener noreferrer');

  // Accept one as a team convention; skip the other.
  await sug.getByRole('button', { name: 'Create rule…' }).click();
  const ruleDialog = page.getByRole('dialog', { name: 'Add a team convention' });
  await expect(ruleDialog.getByLabel('Convention')).toHaveValue('Add a regression test with every bug fix');
  await expect(ruleDialog.getByLabel('Reason')).toHaveValue('Reviewers asked for this in 3 merged pull requests');
  await ruleDialog.getByLabel(/My team/).check();
  await ruleDialog.getByRole('button', { name: 'Save rule' }).click();
  await expect(tab.locator('.mem-group').first()).toContainText('Add a regression test with every bug fix');
  await expect(tab.locator('.mem-sug', { hasText: 'Add a regression test' })).toHaveCount(0);
  const other = tab.locator('.mem-sug', { hasText: 'Return errors with context' });
  await other.getByRole('button', { name: 'Not now' }).click();
  await expect(other).toHaveCount(0);
});
