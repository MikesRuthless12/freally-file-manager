/// <reference types="svelte" />
/// <reference types="vite/client" />

/**
 * App version, injected by Vite from `package.json` (see the `define`
 * block in `vite.config.ts`). Mirrors the Rust side's
 * `concat!("v", env!("CARGO_PKG_VERSION"))` so the two cannot drift —
 * a hardcoded version here once pointed the offload wizard at a release
 * artifact that did not exist.
 */
declare const __APP_VERSION__: string;
