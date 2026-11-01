import { defineConfig, devices } from "@playwright/test";

// Journeys through the app at an iPhone's size, against the built app with
// the API answered by each test, so they need no treetop serve and no pool.
// Run them in Playwright's image where the host has no browser libraries:
//   docker run --rm --network host --user "$(id -u):$(id -g)" -e HOME=/tmp \
//     -v "$PWD":/work -w /work \
//     mcr.microsoft.com/playwright:v1.63.0-noble npm run e2e
export default defineConfig({
  testDir: "e2e",
  outputDir: "test-results",
  fullyParallel: true,
  retries: 0,
  use: {
    ...devices["iPhone 15"],
    defaultBrowserType: "chromium",
    baseURL: "http://127.0.0.1:4173",
    serviceWorkers: "block",
  },
  webServer: {
    command: "npx vite preview --host 127.0.0.1 --port 4173 --strictPort",
    url: "http://127.0.0.1:4173",
    reuseExistingServer: false,
  },
});
