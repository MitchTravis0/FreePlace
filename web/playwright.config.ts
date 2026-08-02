import { defineConfig } from "@playwright/test";

// Phase 5 browser smoke. The dev server port is fixed so the spec can build
// the page URL; the node under test is the isolated dev node on 7510 (see
// scripts/phase5-smoke.sh).
export default defineConfig({
  testDir: "tests",
  timeout: 120_000,
  use: {
    baseURL: "http://localhost:5210",
  },
  webServer: {
    command: "npx vite --port 5210 --strictPort",
    url: "http://localhost:5210",
    reuseExistingServer: true,
    timeout: 30_000,
  },
});
