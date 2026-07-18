import { defineConfig } from "vitest/config";
import { svelte } from "@sveltejs/vite-plugin-svelte";
import { fileURLToPath } from "node:url";

// Unit-test harness (File Manager ships only Playwright e2e otherwise). Used by
// the "More Freally apps" embed smoke — proves the vendored panel renders and
// localizes through the Fluent i18n bridge. Not wired into CI.
export default defineConfig({
  plugins: [svelte({ hot: false })],
  test: {
    environment: "jsdom",
    include: ["tests/unit/**/*.test.ts"],
    globals: false,
    setupFiles: ["./tests/unit/setup.ts"],
  },
  resolve: {
    conditions: ["browser"],
    // The vendored panel is out-of-tree (repo-root vendor/); dedupe forces its
    // bare imports onto this project's single installed copy.
    dedupe: ["react", "react-dom", "@tauri-apps/api"],
    alias: {
      "@freally/central-panel": fileURLToPath(
        new URL("../../vendor/freally-central/ui/src/panel", import.meta.url),
      ),
    },
  },
  esbuild: { jsx: "automatic", jsxImportSource: "react" },
});
