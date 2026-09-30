import { test, expect } from '@playwright/test';

test.describe('Security & XSS Canaries', () => {
  test('markdown preview neutralizes scripts, hostile attributes, and unescaped HTML', async ({ page }) => {
    await page.goto('/web/index.html?mock=1');
    await expect(page.locator('#app')).not.toHaveAttribute('aria-busy', 'true', { timeout: 10_000 });

    // Install window error spy and canary flags
    await page.evaluate(() => {
      window.__canary_xss = false;
      window.__canary_alert = false;
      window.alert = () => { window.__canary_alert = true; };
    });

    // Open README.md in markdown preview
    await page.keyboard.press('ControlOrMeta+p');
    const input = page.locator('.pal-input');
    await input.fill('README.md');
    await page.waitForTimeout(100);
    await page.keyboard.press('Enter');

    const mdView = page.locator('.md-view');
    await expect(mdView).toBeVisible({ timeout: 10_000 });

    // Inject and test markdown with canary attacks via mock renderer evaluation
    const canaryResults = await page.evaluate(async () => {
      const { renderMarkdown } = await import('/web/src/mock/markdown.js');
      const hostileMarkdown = `
# Canary Document

<script>window.__canary_xss = true;</script>
<img src="invalid-url" onerror="window.__canary_xss = true;">
<a href="javascript:alert(1)">click me</a>
[External](https://evil.example.com/exploit)
`;
      const rendered = renderMarkdown(hostileMarkdown, { path: 'canary.md', rawUrl: (p) => p });
      const container = document.createElement('div');
      // Set through trusted HTML
      const { setTrustedHTML } = await import('/web/src/core/dom.js');
      setTrustedHTML(container, rendered.html, 'markdown');
      document.body.appendChild(container);

      // Give microtasks and events time to potentially fire
      await new Promise((r) => setTimeout(r, 100));

      const hasUnescapedScript = container.querySelector('script') !== null;
      const externalLink = container.querySelector('a[href^="https://"]');
      const rel = externalLink?.getAttribute('rel');
      const xssTriggered = window.__canary_xss || window.__canary_alert;

      container.remove();
      return { hasUnescapedScript, rel, xssTriggered };
    });

    expect(canaryResults.hasUnescapedScript).toBe(false);
    expect(canaryResults.xssTriggered).toBe(false);
    expect(canaryResults.rel).toBe('noopener noreferrer');
  });
});
