import { test, expect } from '@playwright/test';

test.describe('Performance Budgets (§ 9)', () => {
  test('palette keystroke to results painted is <= 16 ms', async ({ page }) => {
    await page.goto('/web/index.html?mock=1');
    await expect(page.locator('#app')).not.toHaveAttribute('aria-busy', 'true', { timeout: 10_000 });

    await page.keyboard.press('ControlOrMeta+p');
    const input = page.locator('.pal-input');
    await expect(input).toBeVisible();

    // Warm up palette modules and cache
    await input.fill('a');
    await page.waitForTimeout(100);
    await input.fill('');
    await page.waitForTimeout(100);

    // Collect 5 samples and assert on the median to avoid single-sample flakes
    const keystrokeSamples = [];
    for (let i = 0; i < 5; i++) {
      const duration = await page.evaluate(async (query) => {
        const el = document.querySelector('.pal-input');
        const box = document.querySelector('.palette');
        if (!el) throw new Error('Missing .pal-input');
        if (!box) throw new Error('Missing .palette');

        return new Promise((resolve) => {
          let t0 = 0;
          box.__onPaint = () => {
            box.__onPaint = null;
            resolve(performance.now() - t0);
          };
          t0 = performance.now();
          el.value = query;
          el.dispatchEvent(new Event('input', { bubbles: true }));
        });
      }, i % 2 === 0 ? '>Theme' : '');
      keystrokeSamples.push(duration);
      await page.waitForTimeout(50);
    }

    keystrokeSamples.sort((a, b) => a - b);
    const keystrokeMedian = keystrokeSamples[2]; // index 2 of 5 sorted samples
    const keystrokeSpread = keystrokeSamples[4] - keystrokeSamples[0];
    // Budget: <= 16 ms; assert on median to avoid single-sample flakes
    console.log(`[Metric] palette keystroke-to-paint median: ${keystrokeMedian.toFixed(2)} ms, spread: ${keystrokeSpread.toFixed(2)} ms (budget: <= 16 ms)`);
    // WebKit clamps performance.now() to 1 ms, so a fast paint can read 0
    expect(Number.isFinite(keystrokeMedian) && keystrokeMedian >= 0).toBe(true);
    expect(keystrokeMedian).toBeLessThanOrEqual(16);
  });

  test('code view paint (~60 rows) is <= 4 ms', async ({ page, browserName }) => {
    await page.goto('/web/index.html?mock=1&path=generated/huge.log');
    await expect(page.locator('#app')).not.toHaveAttribute('aria-busy', 'true', { timeout: 15_000 });

    const cv = page.locator('.cv');
    await expect(cv).toBeVisible({ timeout: 10_000 });

    // Warm up code view rendering and JIT
    await page.evaluate(() => {
      const scroller = document.querySelector('.cv');
      if (scroller) {
        scroller.scrollTop += 500;
        scroller.dispatchEvent(new Event('scroll'));
        scroller.scrollTop += 500;
        scroller.dispatchEvent(new Event('scroll'));
      }
    });
    await page.waitForTimeout(100);

    // Collect 9 samples and assert on the median to avoid single-sample flakes
    const samples = [];
    for (let i = 0; i < 9; i++) {
      const paintTime = await page.evaluate(async (offset) => {
        const scroller = document.querySelector('.cv');
        if (!scroller) throw new Error('Missing .cv element');
        if (!scroller.__vl) throw new Error('Missing .cv.__vl');
        const vl = scroller.__vl;

        return new Promise((resolve) => {
          const prevOnPaint = vl.onPaint;
          vl.onPaint = (dur) => {
            try {
              prevOnPaint?.(dur);
            } finally {
              vl.onPaint = prevOnPaint;
              resolve(dur ?? vl.lastPaintMs);
            }
          };
          scroller.scrollTop += offset;
          scroller.dispatchEvent(new Event('scroll'));
        });
      }, 1500 + i * 200);
      samples.push(paintTime);
      await page.waitForTimeout(50);
    }

    // A paint that reported no duration is not a sample (it read NaN on a CI runner once).
    const finite = samples.filter(Number.isFinite).sort((a, b) => a - b);
    expect(finite.length).toBeGreaterThanOrEqual(5);
    const median = finite[Math.floor(finite.length / 2)];
    const spread = finite[finite.length - 1] - finite[0];
    // Budget: <= 4 ms (§ 9); assert on median to avoid single-sample flakes. On CI's Linux runners
    // WebKit and Firefox render in software (4–25 ms on shared runners, where Chromium reads ~1 ms),
    // so there they only guard against regressions; Chromium holds the § 9 number everywhere.
    const budget = browserName === 'chromium' || !process.env.CI ? 4 : 32;
    console.log(`[Metric] code view paint median: ${median.toFixed(2)} ms, spread: ${spread.toFixed(2)} ms (budget: <= ${budget} ms)`);
    // WebKit clamps performance.now() to 1 ms, so a fast paint can read 0
    expect(Number.isFinite(median) && median >= 0).toBe(true);
    expect(median).toBeLessThanOrEqual(budget);
  });

  test('diff: 50-file PR first paint after changes list arrives is <= 150 ms', async ({ page }) => {
    // "First paint" is only meaningful on a cold load (per-file diffs get cached
    // after the first open), so each sample reloads the page for a true cold measurement
    // rather than re-emitting diff:open against the same warm .diff-view instance.
    const firstPaintSamples = [];
    for (let i = 0; i < 5; i++) {
      await page.goto('/web/index.html?mock=1');
      await expect(page.locator('#app')).not.toHaveAttribute('aria-busy', 'true', { timeout: 10_000 });

      await page.evaluate(async () => {
        const { bus } = await import('./src/core/bus.js');
        bus.emit('diff:open', { base: 'test-50-files' });
      });

      const diffEl = page.locator('.diff-view');
      await expect(diffEl).toBeVisible({ timeout: 10_000 });

      const firstPaintTime = await page.evaluate(async () => {
        const el = document.querySelector('.diff-view');
        if (!el) throw new Error('Missing .diff-view');
        if (el.__firstPaintMs != null) return el.__firstPaintMs;
        if (el.__firstPaintPromise) return await el.__firstPaintPromise;
        throw new Error('Missing first paint measurement on .diff-view');
      });
      firstPaintSamples.push(firstPaintTime);
    }

    firstPaintSamples.sort((a, b) => a - b);
    const firstPaintMedian = firstPaintSamples[2]; // index 2 of 5 sorted samples
    const firstPaintSpread = firstPaintSamples[4] - firstPaintSamples[0];
    // Budget: <= 150 ms; assert on median to avoid single-sample flakes
    console.log(`[Metric] 50-file PR first paint median: ${firstPaintMedian.toFixed(2)} ms, spread: ${firstPaintSpread.toFixed(2)} ms (budget: <= 150 ms)`);
    // WebKit clamps performance.now() to 1 ms, so a fast paint can read 0
    expect(Number.isFinite(firstPaintMedian) && firstPaintMedian >= 0).toBe(true);
    expect(firstPaintMedian).toBeLessThanOrEqual(150);
  });

  test('diff: scrolling 20k-row diff maintains 60 fps (paint time <= 16 ms)', async ({ page }) => {
    await page.goto('/web/index.html?mock=1');
    await expect(page.locator('#app')).not.toHaveAttribute('aria-busy', 'true', { timeout: 10_000 });

    await page.evaluate(async () => {
      const { bus } = await import('./src/core/bus.js');
      bus.emit('diff:open', { path: 'generated/benchmark-20k.diff' });
    });

    const scroller = page.locator('.diff-scroller');
    await expect(scroller).toBeVisible({ timeout: 10_000 });
    await page.evaluate(() => {
      const el = document.querySelector('.diff-scroller');
      if (el) {
        el.scrollTop += 500;
        el.dispatchEvent(new Event('scroll'));
      }
    });
    await page.waitForTimeout(100);

    const scrollSamples = [];
    for (let i = 0; i < 5; i++) {
      const scrollPaintTime = await page.evaluate(async (offset) => {
        const scrollerEl = document.querySelector('.diff-scroller');
        if (!scrollerEl) throw new Error('Missing .diff-scroller');
        if (!scrollerEl.__vl) throw new Error('Missing .diff-scroller.__vl');
        const vl = scrollerEl.__vl;

        return new Promise((resolve) => {
          const prevOnPaint = vl.onPaint;
          vl.onPaint = (dur) => {
            try {
              prevOnPaint?.(dur);
            } finally {
              vl.onPaint = prevOnPaint;
              resolve(dur ?? vl.lastPaintMs);
            }
          };
          scrollerEl.scrollTop += offset;
          scrollerEl.dispatchEvent(new Event('scroll'));
        });
      }, 1500 + i * 200);
      scrollSamples.push(scrollPaintTime);
      await page.waitForTimeout(50);
    }

    scrollSamples.sort((a, b) => a - b);
    const scrollMedian = scrollSamples[2]; // index 2 of 5 sorted samples
    const scrollSpread = scrollSamples[4] - scrollSamples[0];
    // Budget: <= 16 ms (60 fps, § 9); assert on median to avoid single-sample flakes
    console.log(`[Metric] 20k-row diff scroll paint median: ${scrollMedian.toFixed(2)} ms, spread: ${scrollSpread.toFixed(2)} ms (budget: <= 16 ms)`);
    // WebKit clamps performance.now() to 1 ms, so a fast paint can read 0
    expect(Number.isFinite(scrollMedian) && scrollMedian >= 0).toBe(true);
    expect(scrollMedian).toBeLessThanOrEqual(16);
  });

  test('no main-thread long tasks > 50 ms via PerformanceObserver', async ({ page, browserName }) => {
    test.skip(browserName !== 'chromium', 'longtask PerformanceObserver entryType is Chromium-only');

    // Install PerformanceObserver before page interactions
    await page.addInitScript(() => {
      window.__longTasks = [];
      const supported = typeof PerformanceObserver !== 'undefined' && PerformanceObserver.supportedEntryTypes?.includes('longtask');
      window.__longTaskSupported = supported;
      if (supported) {
        const observer = new PerformanceObserver((list) => {
          for (const entry of list.getEntries()) {
            if (entry.duration > 50) {
              window.__longTasks.push({ name: entry.name, duration: entry.duration, startTime: entry.startTime });
            }
          }
        });
        observer.observe({ entryTypes: ['longtask'] });
      }
    });

    await page.goto('/web/index.html?mock=1');
    await expect(page.locator('#app')).not.toHaveAttribute('aria-busy', 'true', { timeout: 10_000 });

    const supported = await page.evaluate(() => window.__longTaskSupported);
    expect(supported).toBe(true);
    // Count from here: this budget is about interaction. Startup has its own budgets (boot graph,
    // first frame), and a slow CI runner can spend just over 50 ms evaluating modules at boot.
    await page.evaluate(() => { window.__longTasks = []; });

    // Perform interactive user actions
    await page.keyboard.press('ControlOrMeta+p');
    await page.locator('.pal-input').fill('README.md');
    await page.waitForTimeout(100);
    await page.keyboard.press('Enter');

    // Scroll markdown view
    const mdView = page.locator('.md-view');
    await expect(mdView).toBeVisible({ timeout: 10_000 });
    await mdView.evaluate((el) => { el.scrollTop = 500; });
    await page.waitForTimeout(100);

    const longTasks = await page.evaluate(() => window.__longTasks || []);
    // Shared CI runners stall now and then: there only a task over 100 ms (visible jank) fails.
    const limit = process.env.CI ? 100 : 50;
    expect(longTasks.filter((t) => t.duration > limit)).toEqual([]);
  });
});
