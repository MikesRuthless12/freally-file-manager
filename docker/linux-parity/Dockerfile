# Linux build/test parity for a Windows dev box.
#
# Closes the "you cannot cross-check cfg-gated code locally" gap that
# forced push-and-see round trips. Several code paths are Unix-only and
# never compile, let alone run, on Windows:
#
#   * `#[cfg(unix)]` branches in the elevation + helper crates
#   * `follow_root_links` behaviour over real symlinks
#   * the `#[cfg(unix)]` symlink jail test in freally-server
#   * SFTP / S3 server paths that use unix permissions
#
# Ubuntu 24.04, NOT Debian bookworm: Tauri 2 needs libwebkit2gtk-4.1,
# which bookworm does not carry. Package set and toolchain pin mirror
# `.github/workflows/ci.yml` so a green run here means the same thing.
FROM ubuntu:24.04

ENV DEBIAN_FRONTEND=noninteractive

# The CI workflow strikes Azure from the mirror list because the
# mirrorlist retries Azure first for every one of the hundreds of
# packages webkit2gtk pulls in, paying a timeout each time — that turned
# a 40-second step into an hour and killed two v0.22.0 release attempts.
# Same reasoning applies to a local build.
RUN sed -i 's|http://azure.archive.ubuntu.com/ubuntu|http://archive.ubuntu.com/ubuntu|g' \
      /etc/apt/sources.list /etc/apt/sources.list.d/* 2>/dev/null || true

RUN apt-get update && apt-get install -y --no-install-recommends \
      -o Acquire::Retries=2 -o Acquire::http::Timeout=15 \
      libwebkit2gtk-4.1-dev \
      libayatana-appindicator3-dev \
      librsvg2-dev \
      libssl-dev \
      libxdo-dev \
      patchelf \
      build-essential \
      pkg-config \
      curl \
      ca-certificates \
      git \
    && rm -rf /var/lib/apt/lists/*

# Rust lives under /usr/local rather than /root so a non-root user can
# reach it (see the USER note below).
ENV RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/usr/local/cargo \
    PATH=/usr/local/cargo/bin:$PATH

# Pinned to the same toolchain CI uses.
ARG RUST_VERSION=1.97.1
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
      | sh -s -- -y --default-toolchain ${RUST_VERSION} --component clippy,rustfmt \
    && chmod -R a+w ${RUSTUP_HOME} ${CARGO_HOME}

# Run as a NON-ROOT user.
#
# This is not hygiene, it is correctness. Root bypasses file permissions
# on Linux, so any test asserting that a read-only target *fails* will
# instead succeed and the test reports a false failure. Two did exactly
# that on the first run of this harness:
#
#   readonly_destination_directory_yields_permission_denied
#   skip_all_of_kind_lets_tree_finish_and_logs_three_errors
#
# GitHub's runners are non-root, so those pass in CI — meaning a
# root-running harness disagrees with the thing it exists to predict.
# Ubuntu 24.04 already ships a user at uid 1000 (`ubuntu`), so creating
# another there fails with "UID 1000 is not unique". Reuse it — the point
# is simply "not root", and uid 1000 also matches what the GitHub runner
# uses.
RUN mkdir -p /target \
    && chown -R 1000:1000 /target

USER 1000:1000
WORKDIR /work
