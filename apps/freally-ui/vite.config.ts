import { defineConfig } from "vite";
import { svelte } from "@sveltejs/vite-plugin-svelte";
import { fileURLToPath } from "node:url";

const host = process.env.TAURI_DEV_HOST;

// https://vite.dev/config/
export default defineConfig(async () => ({
  plugins: [svelte()],

  // "More Freally apps" is the React CentralPanel vendored from the
  // freally-central submodule (view-only). Svelte owns the app; the panel is a
  // React island. esbuild's automatic JSX runtime transforms the vendored .tsx;
  // dedupe forces the out-of-tree panel's bare imports onto this project's
  // single installed copy of react/react-dom/@tauri-apps/api.
  resolve: {
    dedupe: ["react", "react-dom", "@tauri-apps/api"],
    alias: {
      "@freally/central-panel": fileURLToPath(
        new URL("../../vendor/freally-central/ui/src/panel", import.meta.url),
      ),
    },
  },
  esbuild: {
    jsx: "automatic",
    jsxImportSource: "react",
  },

  // Tauri prefers a fixed port and clean output.
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    host: host || false,
    hmr: host ? { protocol: "ws", host, port: 1421 } : undefined,
    watch: { ignored: ["**/src-tauri/**"] },
  },

  // Expose only `VITE_` and `TAURI_ENV_*` to the frontend.
  envPrefix: ["VITE_", "TAURI_ENV_*"],

  build: {
    target:
      process.env.TAURI_ENV_PLATFORM === "windows" ? "chrome105" : "safari13",
    minify: !process.env.TAURI_ENV_DEBUG ? "esbuild" : false,
    sourcemap: !!process.env.TAURI_ENV_DEBUG,
    // Phase 28 — the Drop Stack lives in its own Tauri window.
    // Multi-page entry so Vite emits both `index.html` (main) and
    // `dropstack.html` (stack) into `dist/`.
    rollupOptions: {
      input: {
        main: "index.html",
        dropstack: "dropstack.html",
      },
    },
  },
}));
