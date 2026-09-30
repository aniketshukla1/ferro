import { test, expect } from '@playwright/test';

// Agent threads (API.md § 10.7) against the mock harness: first-use choice, multi-turn
// conversation, review and revert per turn, and review drafts sent to the agent in one batch.

async function openAgentTab(page) {
  await page.goto('/web/index.html?mock=1&path=web/src/core/bus.js&line=6');
  await expect(page.locator('#app')).not.toHaveAttribute('aria-busy', 'true', { timeout: 10_000 });
  await page.keyboard.press('ControlOrMeta+Shift+KeyP');
  await page.locator('.pal-input').fill('>Agent Threads');
  await page.keyboard.press('Enter');
  const tab = page.locator('.th');
  await expect(tab).toBeVisible();
  return tab;
}

test('a thread keeps turns; each turn can be reviewed and reverted (mock)', async ({ page }) => {
  const tab = await openAgentTab(page);
  // First use: an explicit choice of agent.
  await tab.locator('.th-agent button', { hasText: 'Claude Code' }).click();
  await expect(tab.locator('.th-agent')).toContainText('Claude Code');

  await tab.locator('.th-input').fill('Guard against a missing type');
  await page.keyboard.press('ControlOrMeta+Enter');
  const turns = tab.locator('.th-turn');
  await expect(turns).toHaveCount(1);
  await expect(turns.first().locator('.th-state.done')).toContainText('finished');
  await expect(turns.first().locator('.th-changed')).toContainText('bus.js');
  await expect(tab.locator('.th-pick option:checked')).toContainText('Guard against a missing type');

  await tab.locator('.th-input').fill('Now add a comment');
  await tab.locator('.th-actions button', { hasText: 'Send' }).click();
  await expect(turns).toHaveCount(2);
  await expect(turns.nth(1).locator('.th-state.done')).toBeVisible();

  // Review opens the diff against that turn's snapshot.
  await turns.first().locator('button', { hasText: 'Review' }).click();
  await expect(page.locator('.agent-bar')).toContainText('changed 1 file');
  await page.locator('.agent-bar button', { hasText: 'Keep' }).click();

  await turns.first().locator('button', { hasText: 'Revert' }).click();
  await page.locator('.dialog', { hasText: 'Revert this turn?' }).locator('button', { hasText: 'Revert' }).click();
  await expect(page.locator('.toast', { hasText: 'Reverted 1 file' })).toBeVisible();
});

test('a running turn can be stopped, and blocks a second send (mock)', async ({ page }) => {
  const tab = await openAgentTab(page);
  await tab.locator('.th-agent button', { hasText: 'Claude Code' }).click();
  await tab.locator('.th-input').fill('slow refactor');
  await page.keyboard.press('ControlOrMeta+Enter');
  await expect(tab.locator('.th-state .spinner')).toBeVisible();
  await expect(tab.locator('.th-actions button', { hasText: 'Send' })).toBeDisabled();
  await tab.locator('.th-actions button', { hasText: 'Stop' }).click();
  await expect(tab.locator('.th-state.cancelled')).toBeVisible();
});

test('review drafts go to the agent as one batch (mock)', async ({ page }) => {
  await page.goto('/web/index.html?mock=1');
  await expect(page.locator('#app')).not.toHaveAttribute('aria-busy', 'true', { timeout: 10_000 });
  // Opt in to a harness up front (the tab's first-use choice is covered above).
  await page.evaluate(async () => {
    const { request } = await import('/web/src/core/api.js');
    await request('harness', { method: 'PUT', body: { id: 'claude' } });
  });
  await page.locator('.cmdbar').click();
  await page.locator('.pal-input').fill('https://github.com/aniketshukla1/ferro/pull/42');
  await page.locator('.pal-item', { hasText: 'pull/42' }).first().click();
  await expect(page.locator('.pr-bar')).toBeVisible({ timeout: 10_000 });
  await page.evaluate(() => {
    const m = window.__ferroMock;
    const now = new Date().toISOString();
    for (const [i, line] of [[1, 6], [2, 11]]) {
      m.repo.drafts.push({ id: `d_b${i}`, path: 'web/src/core/bus.js', line, side: 'RIGHT', body: `Comment ${i}`, source: 'human', stale: false, createdAt: now, updatedAt: now });
    }
    m.emit('drafts', { drafts: m.repo.drafts });
  });
  const fix = page.locator('.pr-agent-btn');
  await expect(fix).toContainText('Fix 2 with agent');
  await fix.click();
  const tab = page.locator('.th');
  await expect(tab.locator('.th-turn')).toHaveCount(1);
  await expect(tab.locator('.th-ctx li')).toHaveCount(2);
  await expect(tab.locator('.th-ctx')).toContainText('Comment 2');
  await expect(tab.locator('.th-pick option:checked')).toContainText('Address 2 review comments');
  await expect(tab.locator('.th-state.done')).toBeVisible();
});
