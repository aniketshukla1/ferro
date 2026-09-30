// Layout spec for ORA-47: the sidebar panels (Changes, Files, Search, Outline) must never scroll
// sideways, never render content past the sidebar's right edge, keep git row action buttons at a
// fixed x position, and never let a short counter/badge wrap onto a second line. Regression guard
// for the board's narrow-sidebar review (200 / 280 / 400 px, light + dark).
import { existsSync } from 'node:fs';
import { test, expect } from '@playwright/test';

const WIDTHS = [200, 280, 400];
const THEMES = ['porcelain', 'graphite']; // light, dark

/** 48 synthetic changes: a mix of deep/short directories and short/long file names, spread
 * across staged, unstaged, and untracked groups, so every truncation edge case is exercised
 * with a stable, deterministic file set (required for reproducible screenshot baselines). */
function syntheticGitStatus() {
  const deepDir = 'apps/services/platform/internal/very/deeply/nested/directory/structure/src';
  const files = [];
  const push = (path, kind) => {
    const f = { path, index: null, worktree: null, untracked: false, conflicted: false };
    if (kind === 'staged') f.index = 'M';
    else if (kind === 'unstaged') f.worktree = 'M';
    else if (kind === 'untracked') f.untracked = true;
    files.push(f);
  };
  // Short dir, short name.
  for (const name of ['a.ts', 'b.rs', 'c.go', 'd.md', 'e.json', 'f.py']) push(`src/${name}`, 'unstaged');
  // Short dir, long name (name must stay readable; only the dir hint truncates away).
  for (let n = 0; n < 8; n++) push(`src/component-with-an-extremely-long-descriptive-name-for-testing-${n}.tsx`, 'unstaged');
  // Deep dir, short name (dir hint should truncate first).
  for (let n = 0; n < 8; n++) push(`${deepDir}/m${n}/index_${n}.ts`, 'staged');
  // Deep dir AND long name (both may need to give something up; must never overflow).
  for (let n = 0; n < 6; n++) push(`${deepDir}/handlers/very-long-handler-file-name-for-stress-testing-${n}.rs`, 'staged');
  // Root files, no directory at all.
  for (const name of ['README.md', 'CHANGELOG.md', 'a-fairly-long-root-level-config-file-name.json']) push(name, 'unstaged');
  // Untracked, mixed depths.
  for (let n = 0; n < 10; n++) push(`generated/artifacts/build-output-${n}/manifest-${n}.json`, 'untracked');
  for (let n = 0; n < 9; n++) push(`docs/notes/draft-${n}.md`, 'untracked');
  expect(files.length).toBeGreaterThanOrEqual(45);
  return {
    branch: 'ORA-47-changes-panel-layout-bugs-at-narrow-widths-plus-an-automated-layout-spec',
    detached: false,
    headSha: 'a'.repeat(40),
    upstream: 'origin/main',
    ahead: 3,
    behind: 1,
    files,
    counts: {
      staged: files.filter((f) => f.index).length,
      unstaged: files.filter((f) => f.worktree && !f.untracked).length,
      untracked: files.filter((f) => f.untracked).length,
      conflicted: 0,
    },
  };
}

async function boot(page) {
  await page.goto('/web/index.html?mock=1');
  await expect(page.locator('#app')).not.toHaveAttribute('aria-busy', 'true', { timeout: 15_000 });
}

async function setSidebarWidth(page, px) {
  await page.evaluate((w) => {
    document.getElementById('app').style.setProperty('--sidebar-w', `${w}px`);
  }, px);
}

async function setTheme(page, id) {
  await page.evaluate(async (themeId) => {
    const { previewTheme } = await import('./src/features/themes.js');
    previewTheme(themeId);
  }, id);
}

async function loadSyntheticChanges(page, status) {
  await page.evaluate(async (s) => {
    const { store } = await import('./src/core/store.js');
    const { bus } = await import('./src/core/bus.js');
    store.set('git', { ...s });
    // The mock event stream sends its own git status shortly after connecting; under load that can
    // land after this call. Re-apply the synthetic set so the rows under test never change.
    bus.on('ev:git', () => store.set('git', { ...s }));
  }, status);
  await page.evaluate(async () => (await import('./src/core/commands.js')).execute('panel.changes'));
  await expect(page.locator('.git-file-row').first()).toBeVisible();
}

/** Returns violations for the checks that must hold for every sidebar panel: no horizontal
 * scroll, nothing rendered past the panel's right edge, no counter/badge wraps onto a second line,
 * no file name truncated while its directory hint still has width, and no clipped button label. */
async function auditPanel(page) {
  return page.evaluate(() => {
    const sidebar = document.querySelector('.sidebar');
    const sbRect = sidebar.getBoundingClientRect();
    const violations = { scroll: [], edge: [], wrap: [], name: [], clip: [] };

    for (const body of sidebar.querySelectorAll('.panel-body, .tree')) {
      if (body.offsetParent === null) continue; // not the visible panel
      if (body.scrollWidth > body.clientWidth + 1) {
        violations.scroll.push({ selector: body.className, scrollWidth: body.scrollWidth, clientWidth: body.clientWidth });
      }
    }

    const edgeSelectors = 'svg.i, .icon-btn, input, select, button, textarea, .gitc, .count, .badge, .lr-actions, .lr-name, .lr-dir, .git-base-select, .git-base-input, .git-char-count';
    for (const el of sidebar.querySelectorAll(edgeSelectors)) {
      if (el.offsetParent === null && el.tagName !== 'INPUT') continue; // hidden (e.g. custom base input)
      const r = el.getBoundingClientRect();
      if (r.width === 0 && r.height === 0) continue;
      if (r.right > sbRect.right + 1) {
        const cls = typeof el.className === 'string' ? el.className : el.getAttribute('class') || '';
        violations.edge.push({ selector: `${el.tagName.toLowerCase()}.${cls}`, right: r.right, sidebarRight: sbRect.right, overflow: r.right - sbRect.right });
      }
    }

    // A block/inline-grid element reports one client rect even when its text wraps inside it,
    // so count distinct line boxes of the text itself through a Range.
    const lineCount = (el) => {
      const range = document.createRange();
      range.selectNodeContents(el);
      const tops = new Set();
      for (const r of range.getClientRects()) if (r.width > 0) tops.add(Math.round(r.top));
      return tops.size;
    };
    for (const el of sidebar.querySelectorAll('.count, .badge, .git-char-count, .gitc, .git-amend-label')) {
      if (el.offsetParent === null) continue;
      const lines = lineCount(el);
      if (lines > 1) violations.wrap.push({ selector: el.className, text: el.textContent, lines });
    }

    // File names stay readable: a name may truncate only once its directory hint has collapsed.
    // A button label may never be clipped by its own box.
    for (const row of sidebar.querySelectorAll('.git-file-row')) {
      const name = row.querySelector('.lr-name');
      const dir = row.querySelector('.lr-dir');
      if (name.scrollWidth > name.clientWidth + 1 && dir.getBoundingClientRect().width > 1) {
        violations.name.push({ name: name.textContent, nameWidth: name.clientWidth, dirWidth: dir.getBoundingClientRect().width });
      }
    }
    for (const btn of sidebar.querySelectorAll('button.btn')) {
      if (btn.offsetParent === null) continue;
      if (btn.scrollWidth > btn.clientWidth + 1) {
        violations.clip.push({ selector: btn.className, scrollWidth: btn.scrollWidth, clientWidth: btn.clientWidth });
      }
    }

    return violations;
  });
}

/** The status letter (.gitc) and the row action buttons (.git-stage-btn/.git-unstage-btn and
 * .git-discard-btn) must each sit at the same x position in every row, regardless of how long the file name or directory hint is. */
async function rowActionXPositions(page) {
  return page.evaluate(() => {
    const stageX = [...document.querySelectorAll('.git-stage-btn, .git-unstage-btn')].map((b) => Math.round(b.getBoundingClientRect().x));
    const discardX = [...document.querySelectorAll('.git-discard-btn')].map((b) => Math.round(b.getBoundingClientRect().x));
    const statusX = [...document.querySelectorAll('.git-file-row .gitc')].map((b) => Math.round(b.getBoundingClientRect().x));
    return { stageX, discardX, statusX };
  });
}

test.describe('Sidebar panel layout (ORA-47)', () => {
  for (const width of WIDTHS) {
    for (const theme of THEMES) {
      test(`Changes panel at ${width}px, ${theme}: no misalignment, scroll, overflow, or wrap`, async ({ page }) => {
        await boot(page);
        await setTheme(page, theme);
        await setSidebarWidth(page, width);
        await loadSyntheticChanges(page, syntheticGitStatus());

        const rowCount = await page.locator('.git-file-row').count();
        expect(rowCount).toBeGreaterThanOrEqual(45);

        const { stageX, discardX, statusX } = await rowActionXPositions(page);
        expect(statusX.length).toBe(rowCount);
        expect(new Set(statusX).size).toBe(1);
        expect(stageX.length).toBeGreaterThan(0);
        expect(new Set(stageX).size).toBe(1);
        expect(discardX.length).toBeGreaterThan(0);
        expect(new Set(discardX).size).toBe(1);

        const violations = await auditPanel(page);
        expect(violations.scroll, JSON.stringify(violations.scroll)).toEqual([]);
        expect(violations.edge, JSON.stringify(violations.edge)).toEqual([]);
        expect(violations.wrap, JSON.stringify(violations.wrap)).toEqual([]);
        expect(violations.name, JSON.stringify(violations.name)).toEqual([]);
        expect(violations.clip, JSON.stringify(violations.clip)).toEqual([]);
      });
    }
  }

  for (const width of WIDTHS) {
    test(`Files, Search, and Outline panels at ${width}px: no scroll or edge overflow`, async ({ page }) => {
      await boot(page);
      await setSidebarWidth(page, width);

      await page.evaluate(async () => (await import('./src/core/commands.js')).execute('panel.files'));
      await expect(page.locator('.tree-row').first()).toBeVisible();
      let v = await auditPanel(page);
      expect(v.scroll, `files ${width}px: ${JSON.stringify(v.scroll)}`).toEqual([]);
      expect(v.edge, `files ${width}px: ${JSON.stringify(v.edge)}`).toEqual([]);

      await page.evaluate(async () => (await import('./src/core/commands.js')).execute('palette.search'));
      await page.locator('.search-input .input').fill('ferro');
      await expect(page.locator('.sr-file').first()).toBeVisible({ timeout: 10_000 });
      v = await auditPanel(page);
      expect(v.scroll, `search ${width}px: ${JSON.stringify(v.scroll)}`).toEqual([]);
      expect(v.edge, `search ${width}px: ${JSON.stringify(v.edge)}`).toEqual([]);
      expect(v.wrap, `search ${width}px: ${JSON.stringify(v.wrap)}`).toEqual([]);

      await page.evaluate(async () => (await import('./src/core/commands.js')).execute('panel.outline'));
      v = await auditPanel(page);
      expect(v.scroll, `outline ${width}px: ${JSON.stringify(v.scroll)}`).toEqual([]);
      expect(v.edge, `outline ${width}px: ${JSON.stringify(v.edge)}`).toEqual([]);
    });
  }

  for (const width of WIDTHS) {
    for (const theme of THEMES) {
      test(`Changes panel screenshot baseline: ${width}px, ${theme}`, async ({ page, browserName }, testInfo) => {
        test.skip(browserName !== 'chromium', 'Screenshot baselines are captured on one deterministic engine.');
        // The UI uses system fonts, so baselines are per-OS. CI must not invent a baseline on the fly
        // (it would pass vacuously); it skips until that OS's PNGs are committed. The geometry checks
        // above are font-independent and always run.
        const name = `changes-${width}-${theme}.png`;
        test.skip(!!process.env.CI && !existsSync(testInfo.snapshotPath(name, { kind: 'screenshot' })),
          `No ${process.platform} baseline committed for ${name}.`);
        await boot(page);
        await setTheme(page, theme);
        await setSidebarWidth(page, width);
        await loadSyntheticChanges(page, syntheticGitStatus());
        await page.locator('.git-file-row').first().waitFor();
        await expect(page.locator('.sidebar')).toHaveScreenshot(name);
      });
    }
  }
});
