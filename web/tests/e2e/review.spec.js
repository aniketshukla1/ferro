import { test, expect } from '@playwright/test';

// Milestone F3 (Review mode). Follows FRONTEND.md § 6.13 and § 10.
async function openPr(page) {
  await page.goto('/web/index.html?mock=1');
  await expect(page.locator('#app')).not.toHaveAttribute('aria-busy', 'true', { timeout: 10_000 });

  // Palette + pasted PR URL is the documented open flow (FRONTEND.md § 6.13 "Open PR").
  await page.locator('.cmdbar').click();
  await page.locator('.pal-input').fill('https://github.com/aniketshukla1/ferro/pull/42');
  const item = page.locator('.pal-item', { hasText: 'pull/42' }).first();
  await expect(item).toBeVisible({ timeout: 5_000 });
  await item.click();

  await expect(page.locator('.pr-bar')).toBeVisible({ timeout: 10_000 });
  await expect(page.locator('.pr-bar .pr-number-title')).toContainText('#42');
  // The "Reviewing ..." toast lingers a few seconds and can cover later targets; let it clear.
  await expect(page.locator('.toast')).toHaveCount(0, { timeout: 8_000 });
}

test.describe('Milestone F3 (Review mode)', () => {
  test('draft + submit review flow (mock)', async ({ page }) => {
    await openPr(page);

    // Review overview replaces home's normal content once a PR is active.
    const overview = page.locator('.home-review-slot .review-overview');
    await expect(overview).toBeVisible();
    await expect(overview).toContainText('#42');

    // Open the diff from a file row and add an inline comment via the gutter "+".
    await overview.locator('.review-overview-file-row', { hasText: 'search.rs' }).click();
    const diffView = page.locator('.diff-view');
    await expect(diffView).toBeVisible();

    const row = diffView.locator('.diff-row.split').first();
    const newCell = row.locator('.diff-cell.new');
    await newCell.hover();
    await newCell.locator('.diff-comment-add-btn').click({ force: true });

    const composer = page.locator('.composer-card');
    await expect(composer).toBeVisible();
    await composer.locator('.composer-textarea').fill('Please add a regression test for this branch.');
    await composer.locator('button', { hasText: 'Save draft' }).click();

    // Draft renders inline (edit/delete) and the PR bar's submit count updates.
    await expect(page.locator('.draft-card')).toBeVisible();
    await expect(page.locator('.pr-submit-btn')).toContainText('(1)');

    // Submit the review.
    await page.locator('.pr-submit-btn').click();
    const dialog = page.locator('.dialog');
    await expect(dialog).toBeVisible();
    await expect(dialog.locator('.submit-drafts-list')).toContainText('1 draft');
    await dialog.locator('input[value="APPROVE"]').check();
    await dialog.locator('button', { hasText: 'Submit review' }).click();

    await expect(page.locator('.toast', { hasText: 'Review submitted' })).toBeVisible({ timeout: 5_000 });
    await expect(dialog).toBeHidden();
    await expect(page.locator('.pr-submit-btn')).toContainText('(0)');
  });

  test('reply thread and viewed tracking (mock)', async ({ page }) => {
    await openPr(page);
    await page.locator('.review-overview-file-row', { hasText: 'search.rs' }).click();

    const diffView = page.locator('.diff-view');
    await expect(diffView).toBeVisible();

    // The outdated fixture thread (search.rs, old line 10) is collapsed behind its badge.
    const outdated = page.locator('.thread-card.outdated');
    await expect(outdated.locator('.thread-badge-outdated', { hasText: 'Outdated' })).toBeVisible();
    await expect(outdated.locator('.thread-comments')).toBeHidden();

    // Fixture thread on search.rs:12 (RIGHT) renders inline and accepts a reply. Annotation
    // heights are measured, so real clicks land on the card (no overlap with the next rows).
    const thread = page.locator('.thread-card:not(.outdated)').first();
    await expect(thread).toBeVisible();
    await thread.locator('.thread-reply-input').fill('Sounds good, will do.');
    await thread.locator('button', { hasText: 'Reply' }).click();
    await expect(thread.locator('.thread-comment')).toHaveCount(2);
    const box = await thread.boundingBox();
    const next = await page.locator('.diff-row', { hasText: 'let fuzzy = true;' }).first().boundingBox();
    expect(next.y).toBeGreaterThanOrEqual(box.y + box.height - 1);

    // Viewed goes through /review/viewed in PR mode: the PR bar's progress counts it.
    const viewedChk = diffView.locator('.diff-viewed-chk').first();
    await expect(page.locator('.pr-viewed-progress')).toContainText('1/3');
    await viewedChk.check();
    await expect(viewedChk).toBeChecked();
    await expect(page.locator('.pr-viewed-progress')).toContainText('2/3');
  });

  test('headMoved banner refreshes the PR (mock)', async ({ page }) => {
    await openPr(page);
    await page.evaluate(() => {
      const m = window.__ferroMock;
      m.repo.pr = { ...m.repo.pr, headMoved: true };
      m.emit('pr', m.repo.pr);
    });
    const banner = page.locator('.pr-head-moved-banner');
    await expect(banner).toBeVisible();
    await expect(banner).toContainText('New commits pushed');
    await banner.locator('.pr-refresh-btn').click();
    await expect(banner).toBeHidden();
    await expect(page.locator('.toast', { hasText: 'Refreshed PR' })).toBeVisible();
  });

  test('drafts from another tab render inline; an open composer keeps its text (mock)', async ({ page }) => {
    await openPr(page);
    await page.locator('.review-overview-file-row', { hasText: 'search.rs' }).click();
    const diffView = page.locator('.diff-view');
    await expect(diffView.locator('.diff-row.split').first()).toBeVisible();

    // Alt+R on the hovered line opens the composer (Option+R types "®" on macOS: key code).
    await diffView.locator('.diff-row', { hasText: 'let fuzzy = true;' }).first().locator('.diff-cell.new .diff-code').hover();
    await page.keyboard.press('Alt+KeyR');
    const composer = page.locator('.composer-card');
    await expect(composer).toContainText('line 13');
    await composer.locator('.composer-textarea').fill('half-typed thought');

    // Another tab saves a draft: the `drafts` event lands here.
    await page.evaluate(() => {
      const m = window.__ferroMock;
      const now = new Date().toISOString();
      m.repo.drafts.push({ id: 'd_other', path: 'crates/ferro-core/src/search.rs', line: 11, side: 'RIGHT', body: 'From the other tab', source: 'human', stale: false, createdAt: now, updatedAt: now });
      m.emit('drafts', { drafts: m.repo.drafts });
    });
    await expect(page.locator('.draft-card', { hasText: 'From the other tab' })).toBeVisible();
    await expect(page.locator('.pr-submit-btn')).toContainText('(1)');
    await expect(composer.locator('.composer-textarea')).toHaveValue('half-typed thought');

    // Collapsing the file drops its rows from the view; expanding it brings the text back.
    const header = diffView.locator('.diff-file-header', { hasText: 'search.rs' }).first();
    await header.locator('.diff-collapse-btn').click();
    // Released rows stay pooled (hidden) in the virtual list, so count only visible composers.
    await expect(page.locator('.composer-card:visible')).toHaveCount(0);
    await header.locator('.diff-collapse-btn').click();
    await expect(page.locator('.composer-card .composer-textarea')).toHaveValue('half-typed thought');
  });

  test('submit without a token shows the auth hint (mock)', async ({ page }) => {
    await openPr(page);
    await page.evaluate(() => {
      const m = window.__ferroMock;
      m.repo.pr = { ...m.repo.pr, auth: { ...m.repo.pr.auth, hasToken: false } };
    });
    await page.locator('.pr-submit-btn').click();
    const dialog = page.locator('.dialog');
    await dialog.locator('button', { hasText: 'Submit review' }).click();
    await expect(dialog.locator('.submit-auth-hint')).toContainText('gh auth login');
    await expect(page.locator('.toast', { hasText: 'Submit failed' })).toBeVisible();
  });

  test('XSS canary: comment bodies render as text, never HTML', async ({ page }) => {
    await page.goto('/web/index.html?mock=1');
    await expect(page.locator('#app')).not.toHaveAttribute('aria-busy', 'true', { timeout: 10_000 });

    await page.evaluate(() => { window.__canary_xss = false; });

    const canary = await page.evaluate(async () => {
      const { createInlineThread } = await import('/web/src/features/review.js');
      window.__canary_xss = false;
      const thread = {
        id: 'th_canary', path: 'x', line: 1, side: 'RIGHT', outdated: false, resolved: false,
        comments: [{
          id: 'c_canary', author: { login: 'attacker' },
          body: '<script>window.__canary_xss = true;</script><img src=x onerror="window.__canary_xss=true">',
        }],
      };
      const el = createInlineThread(thread);
      document.body.appendChild(el);
      await new Promise((r) => setTimeout(r, 50));
      const hasScript = el.querySelector('script') !== null;
      const hasImg = el.querySelector('img') !== null;
      const text = el.textContent;
      el.remove();
      return { hasScript, hasImg, text, xssTriggered: window.__canary_xss };
    });

    expect(canary.hasScript).toBe(false);
    expect(canary.hasImg).toBe(false);
    expect(canary.xssTriggered).toBe(false);
    expect(canary.text).toContain('<script>window.__canary_xss = true;</script>');
  });
});
