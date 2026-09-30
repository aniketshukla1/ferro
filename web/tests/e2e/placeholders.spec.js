import { test, expect } from '@playwright/test';

test.describe('Milestone F2 (Changes, Git Panel & Diff View)', () => {
  test('F2: split and unified diff with intraline markers and hunk expanders', async ({ page }) => {
    await page.goto('/web/index.html?mock=1');
    await expect(page.locator('#app')).not.toHaveAttribute('aria-busy', 'true', { timeout: 10_000 });

    // Open changes panel and then diff view
    await page.locator('.sw-btn[aria-label="Changes"]').click();
    await expect(page.locator('.git-panel-body')).toBeVisible();

    const searchRow = page.locator('.git-file-row', { hasText: 'search.rs' });
    await expect(searchRow).toBeVisible();
    await searchRow.click();

    const diffView = page.locator('.diff-view');
    await expect(diffView).toBeVisible();

    // Split layout by default
    await expect(diffView.locator('.diff-row.split').first()).toBeVisible();
    const splitRow = diffView.locator('.diff-row.split').first();
    await expect(splitRow.locator('.diff-cell.old')).toBeVisible();
    await expect(splitRow.locator('.diff-cell.new')).toBeVisible();

    // Intraline highlights via CSS.highlights
    const hasHighlightApi = await page.evaluate(() => typeof CSS !== 'undefined' && !!CSS.highlights);
    if (hasHighlightApi) {
      const hasHl = await page.evaluate(() => CSS.highlights.has('ferro-diff-add') || CSS.highlights.has('ferro-diff-del'));
      expect(hasHl).toBe(true);
    }

    // Unified mode toggle
    const unifiedBtn = diffView.locator('.diff-layout-btn[data-layout="unified"]');
    await unifiedBtn.click();
    await expect(diffView.locator('.diff-row.unified').first()).toBeVisible();
    await expect(diffView.locator('.diff-row.unified .diff-sign').first()).toBeVisible();

    // Switch back to split
    await diffView.locator('.diff-layout-btn[data-layout="split"]').click();
    await expect(diffView.locator('.diff-row.split').first()).toBeVisible();

    // Whitespace toggle
    const wsBtn = diffView.locator('.diff-ws-btn');
    await wsBtn.click();
    await expect(wsBtn).toHaveClass(/active/);
    await wsBtn.click();
    await expect(wsBtn).not.toHaveClass(/active/);

    // Hunk expander (the list keeps released items pooled and hidden: pick a visible one)
    const hunkSep = diffView.locator('.diff-item:not([hidden]) .diff-hunk-sep').first();
    await expect(hunkSep).toBeVisible();
    const itemsCountBefore = await page.evaluate(() => document.querySelector('.diff-scroller').__vl.count);
    await hunkSep.locator('.diff-expand-up-btn').click();
    await expect.poll(async () => page.evaluate(() => document.querySelector('.diff-scroller').__vl.count)).toBeGreaterThan(itemsCountBefore);
  });

  test('F2: file header actions, collapse, viewed status, and keyboard shortcuts', async ({ page }) => {
    await page.goto('/web/index.html?mock=1');
    await expect(page.locator('#app')).not.toHaveAttribute('aria-busy', 'true', { timeout: 10_000 });

    await page.locator('.sw-btn[aria-label="Changes"]').click();
    await expect(page.locator('.git-panel-body')).toBeVisible();
    const searchRow = page.locator('.git-file-row[title="crates/ferro-core/src/search.rs"]');
    await expect(searchRow).toBeVisible();
    await searchRow.click();

    const diffView = page.locator('.diff-view');
    await expect(diffView).toBeVisible();
    await expect(diffView.locator('.diff-toolbar-info')).toContainText('changed', { timeout: 10_000 });

    const fileHeader = diffView.locator('.diff-file-header', { hasText: 'crates/ferro-core/src/search.rs' });
    await expect(fileHeader).toBeVisible({ timeout: 10_000 });
    await expect(fileHeader.locator('.diff-add-stat')).toBeVisible();
    await expect(fileHeader.locator('.diff-del-stat')).toBeVisible();

    // Toggle collapse
    const collapseBtn = fileHeader.locator('.diff-collapse-btn');
    await expect(collapseBtn).toHaveAttribute('aria-expanded', 'true');
    await collapseBtn.click();
    await expect(collapseBtn).toHaveAttribute('aria-expanded', 'false');
    // Uncollapse
    await collapseBtn.click();
    await expect(collapseBtn).toHaveAttribute('aria-expanded', 'true');

    // Viewed checkbox
    const viewedChk = fileHeader.locator('.diff-viewed-chk');
    await expect(viewedChk).not.toBeChecked();
    await viewedChk.check();
    await expect(viewedChk).toBeChecked();

    // Keyboard shortcut 'v' toggles viewed
    await diffView.locator('.diff-scroller').focus();
    await page.keyboard.press('v');
    await expect(viewedChk).not.toBeChecked();

    // Keyboard shortcut 'c' outside PR mode: either no line has been hovered yet (hint toast)
    // or the mouse crossed a diff row on its way here (no-PR toast) — both are the F3 guard,
    // see e2e/review.spec.js for the full in-PR comment flow.
    await page.keyboard.press('c');
    await expect(page.locator('.toast', { hasText: /Point at a line|Comments need an open pull request/ })).toBeVisible();

    // Close diff view with close button
    await diffView.locator('.diff-close-btn').click();
    await expect(diffView).toBeHidden();
  });

  test('F2: special diffs (too-large banner, binary file, and image diff modes)', async ({ page }) => {
    await page.goto('/web/index.html?mock=1');
    await expect(page.locator('#app')).not.toHaveAttribute('aria-busy', 'true', { timeout: 10_000 });

    await page.locator('.sw-btn[aria-label="Changes"]').click();
    await expect(page.locator('.git-panel-body')).toBeVisible();

    // Image diff: favicon.svg
    const imgRow = page.locator('.git-file-row', { hasText: 'favicon.svg' });
    await expect(imgRow).toBeVisible();
    await imgRow.click();
    const diffView = page.locator('.diff-view');
    await expect(diffView).toBeVisible();

    const imgView = diffView.locator('.diff-image-view');
    await expect(imgView).toBeVisible();
    // Default side-by-side mode
    await expect(imgView.locator('.diff-img-side')).toBeVisible();

    // Swipe mode
    await imgView.locator('.diff-img-mode[data-mode="swipe"]').click();
    await expect(imgView.locator('.diff-img-swipe-container')).toBeVisible();

    // Onion-skin mode
    await imgView.locator('.diff-img-mode[data-mode="onion"]').click();
    await expect(imgView.locator('.diff-img-onion-container')).toBeVisible();

    // Binary file diff: test.bin
    const binRow = page.locator('.git-file-row', { hasText: 'test.bin' });
    await expect(binRow).toBeVisible();
    await binRow.click();
    const binBanner = page.locator('.diff-banner.binary');
    await expect(binBanner).toBeVisible();
    await expect(binBanner.locator('a')).toHaveAttribute('href', /api\/v1\/file\/raw/);

    // Too-large diff: huge.diff
    const hugeRow = page.locator('.git-file-row', { hasText: 'huge.diff' });
    await expect(hugeRow).toBeVisible();
    await hugeRow.click();
    const tooLargeBanner = page.locator('.diff-banner.too-large');
    await expect(tooLargeBanner).toBeVisible();
    await expect(tooLargeBanner.locator('.diff-load-large-btn')).toBeVisible();
  });

  test('F2: gutter markers and inline mini-diffs', async ({ page }) => {
    await page.goto('/web/index.html?mock=1&path=crates/ferro-core/src/search.rs');
    await expect(page.locator('#app')).not.toHaveAttribute('aria-busy', 'true', { timeout: 10_000 });

    const codeView = page.locator('.cv');
    await expect(codeView).toBeVisible();

    // Gutter markers applied to line numbers
    const marker = codeView.locator('.gutter-add, .gutter-mod, .gutter-del').first();
    await expect(marker).toBeVisible({ timeout: 5_000 });

    // Click marked gutter line to open inline mini diff
    await marker.click();
    const miniDiff = page.locator('.mini-diff-card');
    await expect(miniDiff).toBeVisible();
    await expect(miniDiff.locator('.mini-diff-head')).toBeVisible();
    await expect(miniDiff.locator('.mini-diff-row').first()).toBeVisible();

    // Close mini diff
    await miniDiff.locator('button[aria-label="Close mini diff"]').click();
    await expect(miniDiff).toBeHidden();
  });

  test('F2: changes view staging, discard dialog, and git controls', async ({ page }) => {
    await page.goto('/web/index.html?mock=1');
    await expect(page.locator('#app')).not.toHaveAttribute('aria-busy', 'true', { timeout: 10_000 });

    await page.locator('.sw-btn[aria-label="Changes"]').click();
    const panel = page.locator('.git-panel-body');
    await expect(panel).toBeVisible();

    // Branch and base selector
    await expect(page.locator('.git-branch-name')).toHaveText('main');
    const baseSelect = panel.locator('.git-base-select');
    await expect(baseSelect).toBeVisible();
    await baseSelect.selectOption('custom');
    const customInput = panel.locator('.git-base-input');
    await expect(customInput).toBeVisible();

    // Commit box & 72-char guide
    const commitInput = panel.locator('.git-commit-input');
    const charGuide = panel.locator('.git-char-count');
    await expect(charGuide).toHaveText('0 / 72');
    await commitInput.fill('feat: new search optimization');
    await expect(charGuide).toHaveText('29 / 72');

    // Amend toggle
    const amendChk = panel.locator('#git-amend');
    await expect(amendChk).not.toBeChecked();
    await panel.locator('.git-amend-label').click();
    await expect(amendChk).toBeChecked();
    await panel.locator('.git-amend-label').click();

    // Stage a file
    const stageBtn = panel.locator('.git-file-row .git-stage-btn').first();
    await stageBtn.click();
    // Should now have a staged group and unstage button
    const unstageBtn = panel.locator('.git-file-row .git-unstage-btn').first();
    await expect(unstageBtn).toBeVisible();

    // AI message button (F4): fills the commit box from /git/commit-message (mock), which needs
    // staged changes
    await commitInput.fill('');
    await panel.locator('.git-ai-btn').click();
    await expect.poll(() => commitInput.inputValue()).not.toBe('');

    // Unstage the file
    await unstageBtn.click();
    await expect(stageBtn).toBeVisible();

    // Discard button shows confirmation dialog
    const discardBtn = panel.locator('.git-file-row .git-discard-btn').first();
    await discardBtn.click();
    const dialog = page.locator('.dialog');
    await expect(dialog).toBeVisible();
    await expect(dialog.locator('.git-discard-dialog')).toBeVisible();
    // Cancel discard
    await dialog.locator('button', { hasText: 'Cancel' }).click();
    await expect(dialog).toBeHidden();

    // Push and Pull buttons
    await page.locator('button[aria-label="Push commits"]').click();
    await expect(page.locator('.toast', { hasText: 'Pushed commits' })).toBeVisible();

    await page.locator('button[aria-label="Pull commits (fast-forward)"]').click();
    await expect(page.locator('.toast', { hasText: 'Pulled commits' })).toBeVisible();

    // Recent commits list
    const recentDetails = panel.locator('.git-recent-details');
    await recentDetails.locator('summary').click();
    await expect(recentDetails.locator('.git-commit-row').first()).toBeVisible();
  });

  test('F2: renamed file shows old → new path in diff header', async ({ page }) => {
    await page.goto('/web/index.html?mock=1');
    await expect(page.locator('#app')).not.toHaveAttribute('aria-busy', 'true', { timeout: 10_000 });

    await page.locator('.sw-btn[aria-label="Changes"]').click();
    await expect(page.locator('.git-panel-body')).toBeVisible();

    // engine.rs row should exist (the renamed file fixture: query.rs → engine.rs)
    const renamedRow = page.locator('.git-file-row', { hasText: 'engine.rs' });
    await expect(renamedRow).toBeVisible({ timeout: 5_000 });
    await renamedRow.click();

    const diffView = page.locator('.diff-view');
    await expect(diffView).toBeVisible();

    // Diff file header for the renamed file must show "query.rs → engine.rs"
    // Use hasText to uniquely target the rename header (multi-file diff may have many headers)
    const fileHeader = diffView.locator('.diff-file-header', { hasText: 'engine.rs' });
    await expect(fileHeader.first()).toBeVisible({ timeout: 10_000 });
    const headerText = await fileHeader.first().textContent();
    expect(headerText).toContain('query.rs');
    expect(headerText).toContain('engine.rs');
    expect(headerText).toContain('→');
  });

  test('F2: emoji line renders in diff view without corruption', async ({ page }) => {
    await page.goto('/web/index.html?mock=1');
    await expect(page.locator('#app')).not.toHaveAttribute('aria-busy', 'true', { timeout: 10_000 });

    await page.locator('.sw-btn[aria-label="Changes"]').click();
    await expect(page.locator('.git-panel-body')).toBeVisible();

    // Open diff for search.rs which has the emoji fixture in its second hunk
    const searchRow = page.locator('.git-file-row', { hasText: 'search.rs' });
    await expect(searchRow).toBeVisible();
    await searchRow.click();

    const diffView = page.locator('.diff-view');
    await expect(diffView).toBeVisible();

    // The emoji row (🚀) should be present and contain the rocket emoji intact
    const emojiRow = diffView.locator('.diff-row', { hasText: '🚀' });
    await expect(emojiRow).toBeVisible({ timeout: 10_000 });
    const rowText = await emojiRow.textContent();
    expect(rowText).toContain('🚀');

    // The CRLF row in the same hunk renders its code without a visible carriage return
    const crlfRow = diffView.locator('.diff-row', { hasText: 'results.push(hit);' });
    await expect(crlfRow).toBeVisible();
    expect(await crlfRow.innerText()).not.toMatch(/\r|\\r|␍/);
    // ...and stays one row tall (a stray CR must not wrap onto a second line)
    const [crlfBox, emojiBox] = [await crlfRow.boundingBox(), await emojiRow.boundingBox()];
    expect(crlfBox.height).toBe(emojiBox.height);
  });
});

// F3 (draft + submit review) is covered in review.spec.js; F4 (AI findings) in ai.spec.js.
