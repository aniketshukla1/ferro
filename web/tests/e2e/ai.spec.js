import { test, expect } from '@playwright/test';

async function openAiTab(page) {
  await page.goto('/web/index.html?mock=1');
  await expect(page.locator('#app')).not.toHaveAttribute('aria-busy', 'true', { timeout: 10_000 });
  await page.locator('#insp-toggle').click();
  await expect(page.locator('.insp-tab', { hasText: 'AI' })).toHaveAttribute('aria-selected', 'true');
  await expect(page.locator('.ai-seg')).toBeVisible({ timeout: 10_000 });
}

test.describe('Milestone F4 (AI)', () => {
  test('ask panel: escape-first streaming, single swap to final markdown, usage footer, cancel', async ({ page }) => {
    await openAiTab(page);

    // FRONTEND.md § 4 rule 7 canary: a script payload arriving mid-stream (the mock echoes the
    // question back in its first token) must render as visible text, never as a live element, and
    // the backend-rendered markdown must replace it exactly once at the end. The mock's timers are
    // fast (a few hundred ms total), so instead of polling from the test side (racy against when
    // tokens vs. the final event land, and against round-trips to the browser), a MutationObserver
    // is installed in-page *before* the request is sent, recording every mutation of the answer
    // element synchronously as it happens — an exact log of the swap, not a guess at its timing.
    await page.evaluate(() => {
      window.__ferroLog = [];
      const obs = new MutationObserver(() => {
        const el = document.querySelector('.ask-turn.assistant:last-child .ask-answer');
        if (!el) return;
        window.__ferroLog.push({ hasP: !!el.querySelector('p'), hasScript: !!el.querySelector('script'), text: el.textContent });
      });
      obs.observe(document.querySelector('.ask-list'), { childList: true, subtree: true, characterData: true });
      window.__ferroObs = obs;
    });

    const payload = '<script>window.__ferroXss = true;</script>';
    await page.locator('.ask-input').fill(payload);
    await page.locator('.ask-send-btn').click();

    const assistantTurn = page.locator('.ask-turn.assistant').last();
    const answerEl = assistantTurn.locator('.ask-answer');
    const usage = assistantTurn.locator('.ask-usage-text');
    await expect(usage).toBeVisible({ timeout: 5_000 }); // usage fires right after final: the stream is done

    const log = await page.evaluate(() => { window.__ferroObs.disconnect(); return window.__ferroLog; });
    expect(log.length).toBeGreaterThan(0);
    expect(log.some((e) => !e.hasP && e.text.includes(payload))).toBe(true); // mini-rendered as text
    expect(log.every((e) => !e.hasScript)).toBe(true); // never a live <script>, at any point
    const pTransitions = log.filter((e, i) => e.hasP && !(log[i - 1]?.hasP)).length;
    expect(pTransitions).toBe(1); // the backend-rendered swap happens exactly once
    expect(log.at(-1).hasP).toBe(true); // it ends swapped, not still mini-rendered

    // Settled: no further mutations once the swap has happened.
    const finalHtml = await answerEl.innerHTML();
    await page.waitForTimeout(300);
    expect(await answerEl.innerHTML()).toBe(finalHtml);
    expect(await page.evaluate(() => window.__ferroXss)).toBeUndefined();

    // Usage footer (§ 6.14): real numbers from the backend response, including cache reads.
    await expect(usage).toContainText('1,180 in');
    await expect(usage).toContainText('240 out');
    await expect(usage).toContainText('860 cache read');

    // Cancel leaves the panel in a clean state. The mock's stream events land on timers a few tens
    // of ms apart, so clicking Send then Stop as two separate Playwright actions can race past the
    // whole stream before the second click lands. Firing both inside one page.evaluate keeps them
    // in the same synchronous browser task, guaranteeing the abort beats every timer callback.
    await page.evaluate(() => {
      const input = document.querySelector('.ask-input');
      input.value = 'another question';
      input.dispatchEvent(new Event('input', { bubbles: true }));
      document.querySelector('.ask-send-btn').click();
      document.querySelector('.ask-stop-btn').click();
    });
    const secondTurn = page.locator('.ask-turn.assistant').last();
    await expect(secondTurn.locator('.ask-cancelled')).toBeVisible();
    await expect(secondTurn.locator('.ask-answer')).toHaveText('');
    await expect(secondTurn.locator('.ask-usage-text')).toHaveCount(0);
    await page.waitForTimeout(400); // stream must not keep producing events after cancel
    await expect(secondTurn.locator('.ask-answer')).toHaveText('');
    await expect(secondTurn.locator('.ask-usage-text')).toHaveCount(0);
    await expect(page.locator('.ask-stop-btn')).toBeHidden();
    await expect(page.locator('.ask-send-btn')).toBeVisible();
    await expect(page.locator('.ask-input')).toBeEnabled();
  });

  test('AI findings accept/dismiss (mock)', async ({ page }) => {
    await openAiTab(page);

    await page.locator('.ai-seg button[data-mode="review"]').click();
    await expect(page.locator('.review-panel')).toBeVisible();
    await page.locator('.review-head button', { hasText: 'Review changes' }).click();

    const dialog = page.locator('.dialog');
    await expect(dialog).toBeVisible();
    await expect(dialog).toContainText('Review changes with AI');
    await dialog.locator('.dialog-foot button', { hasText: 'Review' }).click();
    await expect(dialog).toBeHidden();

    // Findings stream in; the default filters (severity >= medium) hide the mock's nit and low
    // findings, leaving 3 of the 5 fixture findings across 2 files.
    const cards = page.locator('.finding-card');
    await expect(cards).toHaveCount(3, { timeout: 5_000 });
    await expect(page.locator('.review-file-group')).toHaveCount(2);
    await expect(page.locator('.review-summary')).toContainText('finding');

    // Accept the first finding.
    const first = cards.nth(0);
    const firstTitle = await first.locator('.finding-title').textContent();
    await first.getByRole('button', { name: 'Accept', exact: true }).click();
    await expect(first.locator('.finding-state.ok')).toContainText('Accepted');
    await expect(first.locator('.finding-actions')).toHaveCount(0);

    // Edit & accept the second finding.
    const second = cards.nth(1);
    await second.locator('button', { hasText: 'Edit & accept' }).click();
    const editArea = second.locator('.finding-edit textarea');
    await expect(editArea).toBeVisible();
    await editArea.fill('a hand-edited suggestion');
    await second.locator('button', { hasText: 'Save & accept' }).click();
    await expect(second.locator('.finding-state.ok')).toContainText('Accepted');

    // Dismiss the third finding, with a reason.
    const third = cards.nth(2);
    const thirdTitle = await third.locator('.finding-title').textContent();
    await third.locator('button', { hasText: 'Dismiss' }).click();
    const dismissDialog = page.locator('.dialog');
    await expect(dismissDialog).toContainText('Dismiss finding');
    await dismissDialog.locator('textarea').fill('not applicable here');
    await dismissDialog.locator('.dialog-foot button', { hasText: 'Dismiss' }).click();
    await expect(dismissDialog).toBeHidden();

    // The dismissed finding is gone; the two accepted ones remain, showing their accepted state.
    await expect(page.locator('.finding-card')).toHaveCount(2);
    await expect(page.locator('.finding-card', { hasText: thirdTitle })).toHaveCount(0);
    await expect(page.locator('.finding-card .finding-state.ok')).toHaveCount(2);
    expect(firstTitle).not.toBe(thirdTitle);

    // Filters: raising confidence above every remaining finding's score hides them all.
    await page.locator('.review-filter.num').fill('0.99');
    await expect(page.locator('.finding-card')).toHaveCount(0);
    await expect(page.locator('.review-list')).toContainText('No findings match these filters');
    await page.locator('.review-filter.num').fill('0');
    await expect(page.locator('.finding-card')).toHaveCount(2);
  });
});
