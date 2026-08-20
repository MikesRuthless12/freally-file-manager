# Linux parity harness

Runs the CI gates **and the full test suite** on Linux from a Windows dev
box, so `cfg(unix)` code is actually compiled and executed before a push
instead of being discovered by a red CI leg.

Ubuntu 24.04, not Debian bookworm — Tauri 2 needs `libwebkit2gtk-4.1`,
which bookworm does not carry. The apt set and the toolchain pin
(`1.97.1`) mirror `.github/workflows/ci.yml`.

## Run

```bash
docker build -t freally-linux-parity:latest docker/linux-parity

docker run --rm --name freally-parity \
  -v "$PWD:/work" \
  -v freally-linux-target:/target \
  -v freally-cargo-registry:/root/.cargo/registry \
  -e CARGO_TARGET_DIR=/target \
  freally-linux-parity:latest bash /work/docker/linux-parity/run-parity.sh
```

On Windows PowerShell, replace `$PWD` with the absolute repo path:

```powershell
docker run --rm --name freally-parity `
  -v "C:\Users\miken\Desktop\Havoc Software\Freally File Manager:/work" `
  -v freally-linux-target:/target `
  -v freally-cargo-registry:/root/.cargo/registry `
  -e CARGO_TARGET_DIR=/target `
  freally-linux-parity:latest bash /work/docker/linux-parity/run-parity.sh
```

## Why it runs as a non-root user

Root bypasses file permissions on Linux. Any test asserting that a
read-only target *fails* will instead succeed, and the harness reports a
false failure. On this harness's first run, two did:

- `readonly_destination_directory_yields_permission_denied`
- `skip_all_of_kind_lets_tree_finish_and_logs_three_errors`

GitHub's runners are non-root, so both pass in CI — a root-running
harness would disagree with the very thing it exists to predict, and a
harness that cries wolf twice per run is one people stop reading. The
image therefore declares `USER 1000:1000` (Ubuntu 24.04 already has a
user there), and `run-parity.sh` refuses to
run as uid 0 rather than produce a misleading green or red.

If you see the refusal, rebuild the image — an older cached image ran as
root.

## Why the volumes

- `CARGO_TARGET_DIR=/target` on a **named volume** so the Linux build can
  never touch the Windows `target/`. Artifacts built for one platform in
  the other's target dir cause failures that look like source bugs.
- A named volume for the cargo registry so a rerun does not re-download
  the whole index.
- `--locked` inside the script so a container run cannot rewrite
  `Cargo.lock` for the host.

## Reading the result

The script runs every gate even after one fails, and prints
`--- <step> exit=<code>` per step plus a final `EXITCODE=` line. Assert on
that sentinel — **not** on the absence of error lines. A run that never
started (bad mount, image missing) also produces no error lines.

## What this catches that Windows cannot

- `#[cfg(unix)]` branches in the elevation and helper crates.
- `follow_root_links(false)` over real symlinks in the copy engine,
  scanner, and sync walker.
- `freally-server`'s `#[cfg(unix)]` jail test, which asserts a symlink
  under the served root pointing outside it is refused.
- Unix permission behaviour in the SFTP/S3 server paths.
