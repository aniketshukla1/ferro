import { test, expect } from '@playwright/test';

// Problems pane (API.md § 4.9): language-server diagnostics for opened files, against the mock
// server (errors on fuzzy.rs, a warning on text.rs).

async function openAt(page, path, line = 1) {
  await page.goto(`/web/index.html?mock=1&path=${encodeURIComponent(path)}&line=${line}`);
  await expect(page.locator('#app')).not.toHaveAttribute('aria-busy', 'true', { timeout: 10_000 });
  await expect(page.locator('.cv-row.cur')).toBeVisible();
}

test.describe('Problems (language servers)', () => {
  test('opening a file shows its diagnostics: status counts, line markers, Problems tab', async ({ page }) => {
    await openAt(page, 'crates/ferro-core/src/fuzzy.rs', 1);
    const counts = page.locator('.sb-item[data-tip^="Problems"]');
    await expect(counts).toHaveText('1 · 1');
    // The error line's number carries the severity and the message.
    const row = page.locator('.cv-row.diag-error');
    await expect(row).toHaveCount(1);
    await expect(row.locator('.cv-ln')).toHaveText('12');
    await expect(row.locator('.cv-ln')).toHaveAttribute('title', /mismatched types/);

    await counts.click();
    const tab = page.locator('.insp-tab', { hasText: 'Problems' });
    await expect(tab).toHaveAttribute('aria-selected', 'true');
    const pane = page.locator('.pb');
    await expect(pane.locator('.pb-summary')).toHaveText('1 error · 1 warning');
    await expect(pane.locator('.pb-row')).toHaveCount(2);
    await expect(pane.locator('.pb-row').first()).toContainText('mismatched types');
    await expect(pane.locator('.pb-servers')).toContainText('rust (ready)');

    // Warnings filter, then click through to the line.
    await pane.getByRole('button', { name: 'Warnings' }).click();
    await expect(pane.locator('.pb-row')).toHaveCount(1);
    await pane.locator('.pb-row', { hasText: 'unused variable' }).click();
    await expect(page.locator('.cv-row.cur .cv-ln')).toHaveText('30');
  });

  test('diagnostics accumulate across opened files', async ({ page }) => {
    await openAt(page, 'crates/ferro-core/src/fuzzy.rs', 1);
    await expect(page.locator('.sb-item[data-tip^="Problems"]')).toHaveText('1 · 1');
    await page.locator('.cmdbar').click();
    await page.locator('.pal-input').fill('ferro-core/src/text.rs');
    await expect(page.locator('.pal-item').first()).toContainText('text.rs');
    await page.keyboard.press('Enter');
    await expect(page.locator('.sb-item[data-tip^="Problems"]')).toHaveText('1 · 2');
    await expect(page.locator('.cv-row.diag-warning .cv-ln')).toHaveText('4');
  });
});
