## What & why

<!-- What does this change do, and why? -->

## Phase / roadmap item

<!-- e.g. FC-00b / P0.2 -->

## Definition of Done

- [ ] `cargo fmt --check` + `prettier --check` clean
- [ ] `cargo clippy -D warnings` + `cargo test` green (whole workspace)
- [ ] UI `typecheck` + `lint` + `test` green
- [ ] `i18n:lint` — 18-locale parity green
- [ ] Playwright `test:e2e` green (where applicable)
- [ ] `cargo deny check` clean
- [ ] `/code-review` (high) run and every finding fixed
- [ ] `/security-review` run and every finding fixed
- [ ] Docs + CHANGELOG updated; version bumped where applicable
- [ ] CI matrix green on **Windows, macOS, and Linux**
