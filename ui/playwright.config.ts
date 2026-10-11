import { defineConfig, devices } from "@playwright/test";

// The browser tests drive the built console as the service serves it (CSP included), against
// a seeded test service with a scripted librarian (`tests/console_e2e.rs`), which this starts.
export default defineConfig({
  testDir: "tests",
  fullyParallel: false,
  workers: 1,
  timeout: 60_000,
  expect: { timeout: 10_000 },
  reporter: process.env.CI ? "github" : "list",
  use: {
    baseURL: "http://127.0.0.1:8097",
    locale: "zh-CN",
    trace: "retain-on-failure",
  },
  projects: [
    {
      name: "chromium",
      // The hall is drawn with WebGL; a browser without a GPU (CI) draws it in software.
      use: { ...devices["Desktop Chrome"], launchOptions: { args: ["--enable-unsafe-swiftshader"] } },
    },
  ],
  webServer: {
    command: "cargo test --test integration console_e2e:: -- --ignored --nocapture",
    cwd: "..",
    url: "http://127.0.0.1:8097/health",
    timeout: 900_000,
    reuseExistingServer: !process.env.CI,
    stdout: "ignore",
    stderr: "pipe",
  },
});
