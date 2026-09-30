import { test, expect } from '@playwright/test';
import AxeBuilder from '@axe-core/playwright';
import { readThemes } from '../unit/contrast-rules.js';
import { fileURLToPath } from 'node:url';

// Expected --bg-0 of every theme, straight from the stylesheets.
const css = (f) => fileURLToPath(new URL(`../../styles/${f}`, import.meta.url));
const T = readThemes(css('themes.css'), css('themes-extra.css'));
const PACK = ['slate', 'fjord', 'abyss', 'moss', 'umber', 'dusk', 'paper', 'mist', 'sand', 'sage', 'frost', 'dawn', 'chalk'];

const ready = (page) => expect(page.locator('#app')).not.toHaveAttribute('aria-busy', 'true', { timeout: 10_000 });
const bg0 = (page, sel = 'html') => page.locator(sel).first().evaluate((el) => getComputedStyle(el).getPropertyValue('--bg-0').trim());
const packRequested = (page) => page.evaluate(() => performance.getEntriesByType('resource').some((r) => r.name.includes('themes-extra.css')));

async function bootWith(page, theme) {
  // Record the theme of the first rendered frame (rAF runs only once render-blocking CSS is in).
  await page.addInitScript((id) => {
    if (id) localStorage.setItem('ferro.theme', JSON.stringify(id));
    requestAnimationFrame(() => { window.__firstFrame = getComputedStyle(document.documentElement).getPropertyValue('--bg-0').trim(); });
  }, theme);
  await page.goto('/web/index.html?mock=1');
  await ready(page);
}

test.describe('Themes (§ 5.2, § 5.3)', () => {
  test('default boot does not fetch the theme pack', async ({ page }) => {
    await bootWith(page, null);
    expect(['graphite', 'porcelain']).toContain(await page.locator('html').getAttribute('data-theme'));
    expect(await packRequested(page)).toBe(false);
  });

  // Firefox has no `blocking="render"`, so a pack theme's first frame is its base light or dark
  // theme until themes-extra.css lands (boot-theme.js); everywhere else it is the theme itself.
  const BASE_OF = { moss: 'graphite', paper: 'porcelain' };
  for (const id of ['carbon', 'moss', 'paper']) {
    test(`saved ${id} paints in its first frame`, async ({ page, browserName }) => {
      await bootWith(page, id);
      await expect(page.locator('html')).toHaveAttribute('data-theme', id);
      expect(await bg0(page)).toBe(T[id]['bg-0']);
      const first = await page.evaluate(() => window.__firstFrame);
      const ok = [T[id]['bg-0']];
      if (browserName === 'firefox' && BASE_OF[id]) ok.push(T[BASE_OF[id]]['bg-0']);
      expect(ok).toContain(first);
    });
  }

  test('palette theme mode lists all 16 + auto and previews live, Escape restores', async ({ page }) => {
    await bootWith(page, 'graphite');
    await page.locator('.cmdbar').click();
    await page.locator('.pal-input').fill('>Color Theme');
    await page.keyboard.press('Enter');
    await expect(page.locator('.pal-item')).toHaveCount(17);
    for (const id of ['abyss', 'chalk', 'sand']) {
      await page.locator('.pal-input').fill(id);
      await expect(page.locator('.pal-item')).toHaveCount(1);
      await expect(page.locator('html')).toHaveAttribute('data-theme', id);
      expect(await bg0(page)).toBe(T[id]['bg-0']);
      expect(await bg0(page, '.pal-item .swatch')).toBe(T[id]['bg-0']);
    }
    await page.keyboard.press('Escape');
    await expect(page.locator('html')).toHaveAttribute('data-theme', 'graphite');
    expect(await page.evaluate(() => localStorage.getItem('ferro.theme'))).toBe('"graphite"');
  });

  test('settings gallery previews every theme and applies + saves a pick', async ({ page }) => {
    await bootWith(page, 'porcelain');
    await page.locator('button[aria-label="Settings"]').click();
    const cards = page.locator('.theme-card');
    await expect(cards).toHaveCount(17);
    for (const id of Object.keys(T)) {
      const card = page.locator('.theme-card', { has: page.locator('.tc-name', { hasText: new RegExp(`^${id}$`, 'i') }) });
      await expect.poll(() => card.locator('.tc-preview').evaluate((el) => getComputedStyle(el).getPropertyValue('--bg-0').trim())).toBe(T[id]['bg-0']);
    }
    await page.locator('.theme-card', { hasText: 'Dusk' }).click();
    await expect(page.locator('html')).toHaveAttribute('data-theme', 'dusk');
    await expect(page.locator('.theme-card[aria-checked="true"]')).toContainText('Dusk');
    expect(await page.evaluate(() => localStorage.getItem('ferro.theme'))).toBe('"dusk"');
  });

  test('light / dark toggle returns to the last theme on each side', async ({ page }) => {
    await bootWith(page, 'umber');
    await page.locator('button[aria-label="Toggle light and dark"]').click();
    await expect(page.locator('html')).toHaveAttribute('data-theme', 'porcelain');
    await page.locator('button[aria-label="Toggle light and dark"]').click();
    await expect(page.locator('html')).toHaveAttribute('data-theme', 'umber');
    // pick a light pack theme, then toggle twice: dark comes back as umber, light as sage
    await page.evaluate(() => { localStorage.setItem('ferro.lastLight', '"sage"'); });
    await page.locator('button[aria-label="Toggle light and dark"]').click();
    await expect(page.locator('html')).toHaveAttribute('data-theme', 'sage');
    expect(await bg0(page)).toBe(T.sage['bg-0']);
    await page.locator('button[aria-label="Toggle light and dark"]').click();
    await expect(page.locator('html')).toHaveAttribute('data-theme', 'umber');
  });

  // Rendered contrast at the § 5.2 floors: every highlight class ≥ 3:1 and plain code ≥ 4.5:1 on the
  // editor, measured from computed styles (proves each theme's variables reach the code view).
  for (const id of Object.keys(T)) {
    test(`${id}: rendered code meets the § 5.2 contrast floors`, async ({ page }) => {
      await page.addInitScript((t) => localStorage.setItem('ferro.theme', JSON.stringify(t)), id);
      await page.goto('/web/index.html?mock=1&path=crates/ferro-core/src/diff.rs');
      await ready(page);
      await expect(page.locator('.cv .t-k').first()).toBeVisible({ timeout: 10_000 });
      await expect(page.locator('html')).toHaveAttribute('data-theme', id);
      expect(await bg0(page)).toBe(T[id]['bg-0']);
      const out = await page.evaluate(() => {
        const rgb = (c) => c.match(/[\d.]+/g).slice(0, 3).map(Number);
        const lum = (c) => { const [r, g, b] = rgb(c).map((x) => x / 255).map((x) => (x <= 0.03928 ? x / 12.92 : ((x + 0.055) / 1.055) ** 2.4)); return 0.2126 * r + 0.7152 * g + 0.0722 * b; };
        const ratio = (a, b) => { const [x, y] = [lum(a), lum(b)].sort((p, q) => q - p); return (x + 0.05) / (y + 0.05); };
        const probe = document.createElement('span');
        probe.style.color = 'var(--bg-0)';
        document.body.append(probe);
        const bg = getComputedStyle(probe).color;
        probe.remove();
        const worst = {};
        for (const el of document.querySelectorAll('.cv [class^="t-"]')) {
          const r = ratio(getComputedStyle(el).color, bg);
          worst[el.className] = Math.min(worst[el.className] ?? 99, r);
        }
        const row = document.querySelector('.cv .cv-code');
        return { worst, code: ratio(getComputedStyle(row).color, bg) };
      });
      expect(Object.keys(out.worst).length).toBeGreaterThan(3);
      expect(Object.entries(out.worst).filter(([, r]) => r < 3).map(([k, r]) => `${k} ${r.toFixed(2)}`)).toEqual([]);
      expect(out.code).toBeGreaterThanOrEqual(4.5);
    });
  }

  test('axe WCAG 2.1 AA structure checks pass under a pack theme', async ({ page }) => {
    await bootWith(page, 'fjord');
    await page.locator('button[aria-label="Settings"]').click();
    await expect(page.locator('.theme-card')).toHaveCount(17);
    const results = await new AxeBuilder({ page }).withTags(['wcag2a', 'wcag2aa']).disableRules(['color-contrast', 'nested-interactive']).analyze();
    expect(results.violations).toEqual([]);
  });
});
