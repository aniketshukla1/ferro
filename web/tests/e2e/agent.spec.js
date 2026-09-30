import { test, expect } from '@playwright/test';

// F4b edit with agent (FRONTEND.md § 6.14) against the mock harness: first-use choice,
// job, review with per-hunk revert, revert all, keep, failure, and the overlap guard.

async function openBus(page) {
  await page.goto('/web/index.html?mock=1&path=web/src/core/bus.js&line=6');
  await expect(page.locator('#app')).not.toHaveAttribute('aria-busy', 'true', { timeout: 10_000 });
  await expect(page.locator('.cv-row.cur')).toBeVisible();
}

/** Select lines a..b by dragging over the gutter numbers. */
async function selectLines(page, a, b) {
  const box = async (n) => page.locator('.cv-row', { has: page.locator('.cv-ln', { hasText: new RegExp(`^${n}$`) }) }).locator('.cv-ln').boundingBox();
  const [ba, bb] = [await box(a), await box(b)];
  await page.mouse.move(ba.x + ba.width / 2, ba.y + ba.height / 2);
  await page.mouse.down();
  await page.mouse.move(bb.x + bb.width / 2, bb.y + bb.height / 2);
  await page.mouse.up();
}

async function runAgent(page, instruction, { pick = 'Claude Code' } = {}) {
  await page.keyboard.press('Alt+KeyE');
  const dialog = page.locator('.dialog', { hasText: 'Edit with agent' });
  await expect(dialog).toBeVisible();
  await dialog.locator('.agent-instruction').fill(instruction);
  if (pick) await dialog.locator('.agent-harness', { hasText: pick }).click();
  await dialog.locator('button', { hasText: 'Run agent' }).click();
  await expect(dialog).toHaveCount(0);
}

test.describe('Milestone F4b (Edit with agent)', () => {
  test('first use asks for a harness; a missing one cannot be picked (mock)', async ({ page }) => {
    await openBus(page);
    await selectLines(page, 6, 9);
    await page.keyboard.press('Alt+KeyE');
    const dialog = page.locator('.dialog', { hasText: 'Edit with agent' });
    await expect(dialog.locator('.agent-target')).toContainText('web/src/core/bus.js');
    await expect(dialog.locator('.agent-target')).toContainText('lines 6–9');
    await expect(dialog.locator('.agent-pick')).toContainText('Choose the coding agent');
    await expect(dialog.locator('.agent-harness', { hasText: 'Aider' }).locator('input')).toBeDisabled();
    // Running without an instruction or a choice explains what is missing.
    await dialog.locator('button', { hasText: 'Run agent' }).click();
    await expect(dialog.locator('.agent-error')).toHaveText('Write an instruction first.');
    await dialog.locator('.agent-instruction').fill('tidy');
    await dialog.locator('button', { hasText: 'Run agent' }).click();
    await expect(dialog.locator('.agent-error')).toHaveText('Choose a coding agent.');
  });

  test('run → review → revert one hunk → keep (mock)', async ({ page }) => {
    await openBus(page);
    await selectLines(page, 6, 9);
    await runAgent(page, 'guard against a missing type');
    const bar = page.locator('.agent-bar');
    await expect(bar).toContainText('Claude Code changed 1 file');
    await expect(page.locator('.diff-toolbar-info')).toContainText('Before the agent edit');
    const reverts = page.locator('.diff-hunk-action-btn:visible');
    await expect(reverts).toHaveCount(2);
    await reverts.first().click();
    await expect(page.locator('.diff-hunk-action-btn:visible')).toHaveCount(1);
    await bar.locator('button', { hasText: 'Keep' }).click();
    await expect(page.locator('.toast', { hasText: 'Kept the agent’s changes' })).toBeVisible();
    await expect(page.locator('.diff-view')).toBeHidden();

    // The harness is remembered: the next run shows it instead of asking again.
    await page.locator('.cv-row', { hasText: 'emit(type, payload)' }).locator('.cv-ln').click();
    await page.keyboard.press('Alt+KeyE');
    const dialog = page.locator('.dialog', { hasText: 'Edit with agent' });
    await expect(dialog.locator('.agent-current')).toContainText('Claude Code');
    await expect(dialog.locator('.agent-pick')).toBeHidden();
  });

  test('revert all asks first, then restores the files (mock)', async ({ page }) => {
    await openBus(page);
    await selectLines(page, 6, 6);
    await runAgent(page, 'rename the parameter');
    await page.locator('.agent-bar button', { hasText: 'Revert all' }).click();
    const confirm = page.locator('.dialog', { hasText: 'Revert the agent edit?' });
    await confirm.locator('button', { hasText: 'Revert all' }).click();
    await expect(page.locator('.toast', { hasText: 'Reverted 1 file' })).toBeVisible();
    await expect(page.locator('.diff-view')).toBeHidden();
  });

  test('a failed run says so and offers the output (mock)', async ({ page }) => {
    await openBus(page);
    await selectLines(page, 6, 6);
    await runAgent(page, 'this will fail');
    const t = page.locator('.toast', { hasText: 'Agent edit failed' });
    await expect(t).toContainText('agent exited with code 1');
    await t.locator('button', { hasText: 'Show output' }).click();
    await expect(page.locator('.dialog .agent-output')).toContainText('could not apply the edit');
  });

  test('overlapping lines are refused while an edit runs; Cancel stops it (mock)', async ({ page }) => {
    await openBus(page);
    await selectLines(page, 6, 9);
    await runAgent(page, 'slow refactor');
    const progress = page.locator('.toast', { hasText: 'Claude Code is editing bus.js' });
    await expect(progress).toBeVisible();
    await selectLines(page, 8, 8);
    await page.keyboard.press('Alt+KeyE');
    await expect(page.locator('.toast', { hasText: 'An agent is already editing these lines' })).toBeVisible();
    await expect(page.locator('.dialog', { hasText: 'Edit with agent' })).toHaveCount(0);
    await progress.locator('button', { hasText: 'Cancel' }).click();
    await expect(page.locator('.toast', { hasText: 'Agent edit cancelled' })).toBeVisible();
  });
});
