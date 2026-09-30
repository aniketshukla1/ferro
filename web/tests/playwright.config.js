import { defineConfig, devices } from '@playwright/test';
import { resolve } from 'node:path';

const repoRoot = process.cwd().endsWith('web/tests') ? resolve(process.cwd(), '../..') : resolve(process.cwd());

export default defineConfig({
  testDir: resolve(repoRoot, 'web/tests/e2e'),
  fullyParallel: true,
  forbidOnly: !!process.env.CI,
  retries: process.env.CI ? 2 : 0,
  workers: process.env.CI ? 2 : undefined,
  reporter: 'list',
  // Screenshot baselines are keyed by name and OS: the UI renders with system fonts, so a macOS PNG
  // never matches a Linux render. Specs that use toHaveScreenshot() run on chromium only.
  snapshotPathTemplate: '{testDir}/{testFileDir}/__screenshots__/{testFileName}/{arg}-{platform}{ext}',
  expect: {
    toHaveScreenshot: { maxDiffPixelRatio: 0.01, animations: 'disabled' },
  },
  use: {
    baseURL: 'http://127.0.0.1:4173',
    trace: 'on-first-retry',
  },
  projects: [
    {
      name: 'chromium',
      use: { ...devices['Desktop Chrome'] },
    },
    {
      name: 'webkit',
      use: { ...devices['Desktop Safari'] },
    },
    // Firefox runs in CI (Linux) and environments where macOS TCC does not restrict Nightly profile creation
    ...(process.env.CI || process.env.TEST_FIREFOX || process.platform !== 'darwin' ? [{
      name: 'firefox',
      use: { ...devices['Desktop Firefox'] },
    }] : []),
  ],
  webServer: {
    command: 'python3 web/tests/tools/serve.py 4173',
    cwd: repoRoot,
    url: 'http://127.0.0.1:4173/web/index.html?mock=1',
    reuseExistingServer: !process.env.CI,
    timeout: 15_000,
  },
});
