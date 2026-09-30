import { test, expect } from '@playwright/test';

// Checks (API.md § 16) on the mock: breaking-change radar and affected tests.

async function openChecks(page) {
  await page.goto('/web/index.html?mock=1');
  await expect(page.locator('#app')).not.toHaveAttribute('aria-busy', 'true', { timeout: 10_000 });
  await page.locator('.cmdbar').click();
  await page.locator('.pal-input').fill('>Check This Change');
  await expect(page.locator('.pal-item').first()).toContainText('Check This Change');
  await page.keyboard.press('Enter');
  await expect(page.locator('.ck')).toBeVisible();
}

test('the radar lists removed and re-signatured definitions with their callers', async ({ page }) => {
  await openChecks(page);
  await expect(page.locator('.ck-pair')).toHaveText('HEAD → working tree');
  const radar = page.locator('.ck-sec[data-check="breaking"]');
  await expect(radar).toHaveAttribute('data-state', 'fail');
  await expect(radar.locator('.ck-status')).toHaveText('2 risks · 3 API changes');
  const gone = radar.locator('.ck-item', { hasText: 'score_path' });
  await expect(gone).toContainText('removed');
  await expect(gone).toContainText('3 places still use it (2 in other files)');
  await gone.getByRole('button', { name: 'Show callers' }).click();
  await expect(gone.locator('.ck-refs li')).toHaveCount(3);
  const sig = radar.locator('.ck-item', { hasText: 'Ranker::rank' });
  await expect(sig.locator('.ck-sig .add')).toContainText('limit: usize');
  await expect(radar.locator('.ck-item', { hasText: 'normalize' })).toContainText('renamed to normalize_query');
});

test('tests run only after the command is shown, and failures link to their line', async ({ page }) => {
  await openChecks(page);
  const tests = page.locator('.ck-sec[data-check="tests"]');
  await expect(tests.locator('.ck-cmd code')).toHaveText('$ cargo test -p ferro-core -p ferro-server');
  await tests.getByRole('button', { name: 'Run tests…' }).click();
  const dialog = page.getByRole('dialog', { name: 'Run the tests for this change?' });
  await expect(dialog.locator('pre')).toHaveText('cargo test -p ferro-core -p ferro-server');
  await dialog.getByRole('button', { name: 'Run', exact: true }).click();
  await expect(tests).toHaveAttribute('data-state', 'fail');
  await expect(tests.locator('.ck-status')).toHaveText('211 passed · 1 failed · 2 skipped');
  await expect(tests.locator('.ck-fail')).toContainText('fuzzy::tests::ranks_basename_first');
  await tests.getByRole('button', { name: 'crates/ferro-core/src/fuzzy.rs:212' }).click();
  await expect(page.locator('.cv-row.cur .cv-ln')).toHaveText('212');
});

test('the security scan shows added secrets and sinks; the deep scan adds dependency advisories', async ({ page }) => {
  await openChecks(page);
  const sec = page.locator('.ck-sec[data-check="security"]');
  await expect(sec).toHaveAttribute('data-state', 'fail');
  await expect(sec.locator('.ck-status')).toHaveText('2 findings · 1 serious');
  await expect(sec.locator('.ck-item').first()).toContainText('GitHub token');
  await expect(sec.locator('.ck-excerpt').first()).toHaveText('const TOKEN: &str = "ghp_…";');
  await expect(sec.locator('.ck-tools')).toContainText('osv-scanner (installed)');
  await sec.getByRole('button', { name: 'Deep scan…' }).click();
  const dialog = page.getByRole('dialog', { name: 'Run a deep security scan?' });
  await expect(dialog).toContainText('Runs osv-scanner, npm audit');
  await dialog.getByRole('button', { name: 'Scan', exact: true }).click();
  await expect(sec.locator('.ck-status')).toHaveText('3 findings · 1 serious');
  await expect(sec.locator('.ck-item', { hasText: 'time@0.1.43' })).toContainText('osv-scanner');
  await expect(sec.locator('.ck-tools')).toContainText('Deep scan: osv-scanner (ran)');
});

test('the coding agent gets a test-writing task for the change as a new thread', async ({ page }) => {
  await openChecks(page);
  const agentSec = page.locator('.ck-sec[data-check="agent"]');
  await agentSec.getByLabel('Kind of tests').selectOption('unit');
  // No agent chosen yet: the dialog sends the user to pick one (explicit opt-in).
  await agentSec.getByRole('button', { name: 'Write tests…' }).click();
  await page.getByRole('dialog').getByRole('button', { name: 'Choose an agent' }).click();
  await page.locator('.th-agent button', { hasText: 'Claude Code' }).click();
  await expect(page.locator('.th-agent')).toContainText('Claude Code');
  await page.locator('.cmdbar').click();
  await page.locator('.pal-input').fill('>Check This Change');
  await expect(page.locator('.pal-item').first()).toContainText('Check This Change');
  await page.keyboard.press('Enter');
  await agentSec.getByRole('button', { name: 'Write tests…' }).click();
  const dialog = page.getByRole('dialog', { name: 'Write tests with your agent?' });
  await expect(dialog.locator('pre')).toContainText('Write unit and integration tests');
  await expect(dialog.locator('pre')).toContainText('never product code');
  await dialog.getByRole('button', { name: 'Start', exact: true }).click();
  const tab = page.locator('.th');
  await expect(tab).toBeVisible();
  await expect(tab.locator('.th-pick option:checked')).toContainText('Tests for HEAD → working tree');
  await expect(tab.locator('.th-turn').first()).toContainText('Write unit and integration tests');
});

test('coverage says which added lines ran and marks the rest in the diff', async ({ page }) => {
  await openChecks(page);
  const cov = page.locator('.ck-sec[data-check="coverage"]');
  await expect(cov).toHaveAttribute('data-state', 'warn');
  await expect(cov.locator('.ck-status')).toContainText('added lines ran');
  await expect(cov).toContainText('From lcov.info');
  await expect(cov).toContainText('Not in the report (new, or never loaded by a test): crates/ferro-core/src/new_mod.rs');
  // Open the working-tree diff, then show coverage in it.
  const path = await cov.locator('.ck-name').first().textContent();
  await page.evaluate(async (p) => (await import('/web/src/core/bus.js')).bus.emit('diff:open', { base: 'HEAD', target: 'worktree', path: p }), path);
  await expect(page.locator('.diff-row, .diff-cell').first()).toBeVisible();
  await cov.getByLabel('Show coverage in the diff').check();
  await expect(page.locator('.cov-miss').first()).toBeAttached();
  await cov.getByLabel('Show coverage in the diff').uncheck();
  await expect(page.locator('.cov-miss')).toHaveCount(0);
});
