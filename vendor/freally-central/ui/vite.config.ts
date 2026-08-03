import { defineConfig } from "vitest/config";
import react from "@vitejs/plugin-react";

// Tauri drives the dev server on a fixed port; don't clear the screen so Rust
// logs stay visible. See tauri.conf.json build.devUrl.
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
  },
  build: {
    target: "es2022",
    sourcemap: true,
  },
  test: {
    environment: "jsdom",
    setupFiles: ["./src/test/setup.ts"],
    css: false,
    // Unit tests live in src/; e2e/ is Playwright (run via `test:e2e`).
    include: ["src/**/*.{test,spec}.{ts,tsx}"],
    exclude: ["e2e/**", "node_modules/**", "dist/**"],
  },
});
