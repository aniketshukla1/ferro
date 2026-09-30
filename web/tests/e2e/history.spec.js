import { test, expect } from '@playwright/test';

// History (API.md § 6.6–6.10) against the mock: 152 commits on main, a topic branch, a tag.

async function openHistory(page) {
  await page.goto('/web/index.html?mock=1');
  await expect(page.locator('#app')).not.toHaveAttribute('aria-busy', 'true', { timeout: 10_000 });
  await page.getByRole('tab', { name: 'History' }).click();
  await expect(page.locator('.hi-row').first()).toBeVisible();
}

test('history pages, filters by branch and search, and opens a commit', async ({ page }) => {
  await openHistory(page);
  const info = page.locator('.hi-info');
  await expect(info).toHaveText('100+ commits');
  // Scrolling near the end loads the next page.
  await page.locator('.hi-list').evaluate((el) => { el.scrollTop = el.scrollHeight; });
  await expect(info).toHaveText('152 commits');

  await page.locator('.hi-ref-sel').selectOption('__all__');
  await page.locator('.hi-search').fill('scorer');
  await expect(info).toHaveText('2 commits matching');
  await page.locator('.hi-search').fill('@riya');
  await expect(page.locator('.hi-row:visible', { hasText: 'Aniket Shukla' })).toHaveCount(0);
  await expect(page.locator('.hi-row', { hasText: 'Riya Rao' }).first()).toBeVisible();

  await page.locator('.hi-search').fill('');
  await page.locator('.hi-ref-sel').selectOption('feature/search');
  const top = page.locator('.hi-row', { hasText: 'fuzzy scoring by path depth' });
  await expect(top).toBeVisible();
  await top.click();
  const bar = page.locator('.hc-bar');
  await expect(bar).toContainText('feat(search): fuzzy scoring by path depth');
  await expect(bar).toContainText('Why: faster path ranking');
  await expect(page.locator('.diff-toolbar-info')).toContainText('→');
});

test('the commit header folds to its summary line, and stays folded', async ({ page }) => {
  await openHistory(page);
  await page.locator('.hi-ref-sel').selectOption('feature/search');
  await page.locator('.hi-row', { hasText: 'fuzzy scoring by path depth' }).click();
  const bar = page.locator('.hc-bar');
  const body = bar.locator('.hc-body');
  await expect(body).toBeVisible();
  await bar.getByRole('button', { name: 'Show only the summary' }).click();
  await expect(bar).toContainText('fuzzy scoring by path depth');
  await expect(body).toBeHidden();
  await expect(bar.locator('.hc-meta')).toBeHidden();
  // The next commit, and the next visit, open folded too.
  await page.reload();
  await expect(page.locator('#app')).not.toHaveAttribute('aria-busy', 'true', { timeout: 10_000 });
  await page.getByRole('tab', { name: 'History' }).click();
  await page.locator('.hi-ref-sel').selectOption('feature/search');
  await page.locator('.hi-row', { hasText: 'fuzzy scoring by path depth' }).click();
  await expect(bar.locator('.hc-body')).toBeHidden();
  await bar.getByRole('button', { name: 'Show the full commit message' }).click();
  await expect(bar.locator('.hc-body')).toBeVisible();
  await expect(bar.getByRole('button', { name: 'Show only the summary' })).toHaveAttribute('aria-expanded', 'true');
});

test('Cmd/Ctrl-click two commits compares them; the Compare dialog takes any revision', async ({ page }) => {
  await openHistory(page);
  const mod = process.platform === 'darwin' ? 'Meta' : 'Control';
  await page.locator('.hi-row', { hasText: '#399' }).click({ modifiers: [mod] });
  await page.locator('.hi-row', { hasText: 'initial tree' }).click({ modifiers: [mod] });
  const bar = page.locator('.hc-bar');
  await expect(bar).toContainText('Comparing');
  await expect(bar.locator('code').first()).not.toHaveText(await bar.locator('code').nth(1).textContent());

  await page.locator('.cmdbar').click();
  await page.locator('.pal-input').fill('>Compare Revisions');
  await expect(page.locator('.pal-item').first()).toContainText('Compare Revisions');
  await page.keyboard.press('Enter');
  const dialog = page.getByRole('dialog', { name: 'Compare' });
  await dialog.getByLabel('From (older)').fill('v0.1.0');
  await dialog.getByRole('button', { name: 'Compare', exact: true }).click();
  await expect(bar).toContainText('Comparing v0.1.0 → working tree');
});

test('switching branches is explicit and refused over local changes', async ({ page }) => {
  await openHistory(page);
  await page.locator('.hi-ref-sel').selectOption('feature/search');
  await page.getByRole('button', { name: 'Switch to this branch' }).click();
  const dialog = page.getByRole('dialog', { name: 'Switch to feature/search?' });
  await expect(dialog).toContainText('uncommitted changes');
  await dialog.getByRole('button', { name: 'Switch', exact: true }).click();
  await expect(page.locator('.toast', { hasText: 'Commit or stash your changes first' })).toBeVisible();
  await expect(page.locator('.statusbar')).toContainText('main');

  // The status-bar branch opens the branch picker.
  await page.locator('.statusbar .sb-item', { hasText: 'main' }).first().click();
  const picker = page.getByRole('dialog', { name: 'Switch branch' });
  await picker.getByLabel('Filter branches').fill('search');
  await expect(picker.locator('.hb-row')).toHaveCount(1);
});
