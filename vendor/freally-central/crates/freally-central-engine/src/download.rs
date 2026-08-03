// Installer download engine (Phase 4 / FC-30).
//
// Streams one GitHub release asset to a temp directory, reporting real
// byte-level progress over an IPC channel so the UI can render a precise
// percent. Charter invariant (honest numbers): every progress figure is actual
// bytes received against the size the GitHub API published for the asset —
// nothing estimated, nothing seeded.
//
// Verification before use (the Phase 4 DoD): the byte count must equal the
// API-published size exactly, and when the API publishes a `digest` for the
// asset the streamed SHA-256 must match it. A short, oversized, or
// checksum-mismatched download is refused — the partial file never becomes an
// installer the later phases could run.
//
// Only https URLs of the GitHub release-asset shape
// (github.com/<owner>/<repo>/releases/download/<tag>/<file>) are fetched, and
// redirects (GitHub hands assets off to objects.githubusercontent.com) must
// stay on https. The UI can't hand this engine an arbitrary URL to fetch.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tauri::ipc::Channel;
use tauri::State;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// One asset to download. Everything here comes from the GitHub Releases API
/// response the UI already resolved (see ui/src/releases/) — the engine trusts
/// none of it and re-validates before any network or filesystem work.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadRequest {
    /// Catalog app id — keys the download so the UI can cancel it, and so at
    /// most one download per app runs at a time.
    pub id: String,
    /// The asset's `browser_download_url`.
    pub url: String,
    /// The asset's `name` — becomes the on-disk file name (sanitized).
    pub file_name: String,
    /// The asset's `size` in bytes. The download must match it exactly.
    pub expected_size: u64,
    /// The asset's `digest` ("sha256:<hex>") when GitHub published one.
    pub expected_sha256: Option<String>,
}

/// Progress and terminal events streamed to the UI. Exactly one terminal event
/// (`done` / `failed` / `canceled`) ends every download.
#[derive(Debug, Clone, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum DownloadEvent {
    /// The transfer is starting. `resumed_from` > 0 means a previous partial
    /// file was picked up and the byte count continues from there.
    Started { total: u64, resumed_from: u64 },
    /// Real bytes received so far, out of the exact expected total.
    Progress { received: u64, total: u64 },
    /// All bytes arrived; the size/checksum verification is running.
    Verifying,
    /// Verified and moved into place. `sha256` is the digest actually computed
    /// from the received bytes; `checksum_verified` is false when GitHub
    /// published no digest to compare against (size was still verified).
    Done {
        path: String,
        sha256: String,
        checksum_verified: bool,
    },
    /// The download failed. `code` is a stable machine code the UI maps to a
    /// localized message; `detail` is for logs only, never shown raw.
    Failed { code: &'static str, detail: String },
    /// Canceled by the user. The partial file is kept for a future resume.
    Canceled,
}

/// Stable failure codes (the UI maps these to localized messages).
mod fail {
    pub const BAD_REQUEST: &str = "badRequest";
    pub const ALREADY_DOWNLOADING: &str = "alreadyDownloading";
    pub const NETWORK: &str = "network";
    pub const SIZE_MISMATCH: &str = "sizeMismatch";
    pub const SHORT_DOWNLOAD: &str = "shortDownload";
    pub const CHECKSUM_MISMATCH: &str = "checksumMismatch";
    pub const IO: &str = "io";
}

/// Active downloads, keyed by app id: each holds its cancel flag.
#[derive(Default)]
pub struct Downloads(Mutex<HashMap<String, Arc<AtomicBool>>>);

/// One verified installer this session produced, exactly as the engine left it
/// on disk. Only the download pipeline writes entries (after size + checksum
/// verification); the install engine (Phase 5) consumes them. The UI can never
/// name a file to execute — it names an app id, and the path comes from here.
#[derive(Debug, Clone)]
pub struct VerifiedFile {
    pub path: PathBuf,
    pub file_name: String,
    /// The SHA-256 actually computed from the received bytes.
    pub sha256: String,
    /// True only when GitHub published a digest and it matched.
    pub checksum_verified: bool,
    /// The validated source URL (github.com release-asset shape) — the install
    /// engine re-checks its owner against the baked-in trust allowlist.
    pub source_url: String,
}

/// Session registry of verified downloads, keyed by app id.
#[derive(Default)]
pub struct VerifiedDownloads(Mutex<HashMap<String, VerifiedFile>>);

impl VerifiedDownloads {
    fn record(&self, id: &str, file: VerifiedFile) {
        if let Ok(mut map) = self.0.lock() {
            map.insert(id.to_string(), file);
        }
    }

    /// Drop an app's entry (a re-download starting, or a failed re-hash at
    /// install time).
    pub fn remove(&self, id: &str) {
        if let Ok(mut map) = self.0.lock() {
            map.remove(id);
        }
    }

    /// The verified file for an app, if this session produced one.
    pub fn get(&self, id: &str) -> Option<VerifiedFile> {
        self.0.lock().ok().and_then(|map| map.get(id).cloned())
    }
}

/// Removes the download's registry entry on every exit path (including panics).
struct Registration<'a> {
    downloads: &'a Downloads,
    id: String,
}

impl Drop for Registration<'_> {
    fn drop(&mut self) {
        if let Ok(mut map) = self.downloads.0.lock() {
            map.remove(&self.id);
        }
    }
}

/// Start downloading one release asset, streaming progress to `on_event`.
///
/// The returned future resolves when the download reaches a terminal state; the
/// terminal outcome itself is always delivered as an event (the UI drives its
/// state from the channel alone). Concurrent invocations are independent — the
/// bounded queue for Download All lives in the UI.
#[tauri::command]
pub async fn central_start_download(
    request: DownloadRequest,
    on_event: Channel<DownloadEvent>,
    downloads: State<'_, Downloads>,
    verified: State<'_, VerifiedDownloads>,
) -> Result<(), String> {
    // Register (and auto-unregister) the cancel flag, refusing a duplicate.
    let cancel = Arc::new(AtomicBool::new(false));
    {
        let mut map = downloads
            .0
            .lock()
            .map_err(|_| "download registry poisoned".to_string())?;
        if map.contains_key(&request.id) {
            emit(
                &on_event,
                DownloadEvent::Failed {
                    code: fail::ALREADY_DOWNLOADING,
                    detail: format!("a download for {} is already running", request.id),
                },
            );
            return Ok(());
        }
        map.insert(request.id.clone(), Arc::clone(&cancel));
    }
    let _registration = Registration {
        downloads: &downloads,
        id: request.id.clone(),
    };

    // A re-download is about to replace the app's file — whatever was verified
    // before must not stay eligible for install while the bytes change.
    verified.remove(&request.id);

    if let Err((code, detail)) = run_download(&request, &cancel, &on_event, &verified).await {
        emit(&on_event, DownloadEvent::Failed { code, detail });
    }
    Ok(())
}

/// Send an event to the UI. A dropped channel just means the UI went away.
fn emit(channel: &Channel<DownloadEvent>, event: DownloadEvent) {
    let _ = channel.send(event);
}

/// Cancel a running download by app id. The transfer stops at the next chunk;
/// the partial file is kept so a retry can resume.
#[tauri::command]
pub fn central_cancel_download(id: String, downloads: State<'_, Downloads>) {
    if let Ok(map) = downloads.0.lock() {
        if let Some(flag) = map.get(&id) {
            flag.store(true, Ordering::Relaxed);
        }
    }
}

type Fail = (&'static str, String);

/// The whole download pipeline; any error is reported as a `failed` event by
/// the caller. Ok(()) means a terminal event (`done`/`canceled`) was emitted.
async fn run_download(
    request: &DownloadRequest,
    cancel: &AtomicBool,
    on_event: &Channel<DownloadEvent>,
    verified: &VerifiedDownloads,
) -> Result<(), Fail> {
    let url = validate_asset_url(&request.url).map_err(|e| (fail::BAD_REQUEST, e))?;
    // The id names the per-app subdirectory, the asset name the file — both
    // must be plain names (two apps can ship same-named assets; the per-app
    // directory keeps their .part files from colliding).
    let dir_name = safe_file_name(&request.id).map_err(|e| (fail::BAD_REQUEST, e))?;
    let file_name = safe_file_name(&request.file_name).map_err(|e| (fail::BAD_REQUEST, e))?;
    if request.expected_size == 0 {
        return Err((fail::BAD_REQUEST, "expected size is zero".into()));
    }
    let expected_sha256 = match &request.expected_sha256 {
        Some(digest) => Some(
            parse_sha256(digest)
                .ok_or_else(|| (fail::BAD_REQUEST, format!("unparseable digest: {digest}")))?,
        ),
        None => None,
    };

    let dir = download_dir().join(&dir_name);
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| (fail::IO, e.to_string()))?;
    let part_path = dir.join(format!("{file_name}.part"));
    let final_path = dir.join(&file_name);

    // Resume: hash whatever partial file exists (the digest must cover every
    // byte we end up keeping), then ask the server for the remainder. A .part
    // that is already complete (a cancel landed during verification last time)
    // skips the network entirely and goes straight back to verification.
    let mut hasher = Sha256::new();
    let mut received = hash_existing_prefix(&part_path, &mut hasher, request.expected_size)
        .await
        .map_err(|e| (fail::IO, e.to_string()))?;

    if received == request.expected_size {
        emit(
            on_event,
            DownloadEvent::Started {
                total: request.expected_size,
                resumed_from: received,
            },
        );
    } else {
        stream_remainder(
            request,
            url,
            cancel,
            on_event,
            &part_path,
            &mut hasher,
            &mut received,
        )
        .await?;
        if received != request.expected_size {
            // The stream ended early on a user cancel; the terminal event has
            // already been emitted and the .part is kept for resume.
            return Ok(());
        }
    }

    // A cancel that lands after the last chunk must still be honored — the
    // stream loop won't run again to see the flag.
    if cancel.load(Ordering::Relaxed) {
        emit(on_event, DownloadEvent::Canceled);
        return Ok(());
    }

    emit(on_event, DownloadEvent::Verifying);

    let actual_sha256 = hex::encode(hasher.finalize());
    let checksum_verified = match &expected_sha256 {
        Some(expected) => {
            if *expected != actual_sha256 {
                // Corrupted or tampered — refuse and discard, never install.
                let _ = tokio::fs::remove_file(&part_path).await;
                return Err((
                    fail::CHECKSUM_MISMATCH,
                    format!("sha256 {actual_sha256} != published {expected}"),
                ));
            }
            true
        }
        None => false,
    };

    // A cancel during verification stops the file from being placed (the
    // verified .part stays and a retry skips straight back here).
    if cancel.load(Ordering::Relaxed) {
        emit(on_event, DownloadEvent::Canceled);
        return Ok(());
    }

    // Verified — move into place (replacing any older copy of the same asset).
    let _ = tokio::fs::remove_file(&final_path).await;
    tokio::fs::rename(&part_path, &final_path)
        .await
        .map_err(|e| (fail::IO, e.to_string()))?;

    // Record what was verified so the install engine (Phase 5) can resolve this
    // app id to a file it is allowed to execute.
    verified.record(
        &request.id,
        VerifiedFile {
            path: final_path.clone(),
            file_name: file_name.clone(),
            sha256: actual_sha256.clone(),
            checksum_verified,
            source_url: request.url.clone(),
        },
    );

    emit(
        on_event,
        DownloadEvent::Done {
            path: final_path.to_string_lossy().into_owned(),
            sha256: actual_sha256,
            checksum_verified,
        },
    );
    Ok(())
}

/// Fetch the bytes still missing from the partial file, streaming real byte
/// progress. On return, `received == expected_size` means every byte arrived
/// (flushed and synced); anything less means a cancel was emitted and the
/// .part was kept for resume. Transport/verification errors bubble up as Fail.
#[allow(clippy::too_many_arguments)]
async fn stream_remainder(
    request: &DownloadRequest,
    url: reqwest::Url,
    cancel: &AtomicBool,
    on_event: &Channel<DownloadEvent>,
    part_path: &Path,
    hasher: &mut Sha256,
    received: &mut u64,
) -> Result<(), Fail> {
    let client = shared_client()?;
    let mut req = client.get(url);
    if *received > 0 {
        req = req.header(reqwest::header::RANGE, format!("bytes={}-", *received));
    }
    let response = req
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| (fail::NETWORK, e.to_string()))?;

    let resuming = response.status() == reqwest::StatusCode::PARTIAL_CONTENT;
    if resuming {
        // The server must be completing the same file we have the prefix of.
        let total = response
            .headers()
            .get(reqwest::header::CONTENT_RANGE)
            .and_then(|v| v.to_str().ok())
            .and_then(total_from_content_range);
        if total != Some(request.expected_size) {
            return Err((
                fail::SIZE_MISMATCH,
                format!(
                    "resume total {total:?} != expected {}",
                    request.expected_size
                ),
            ));
        }
    } else {
        // A fresh 200 restarts from byte zero even if we asked for a range.
        *received = 0;
        *hasher = Sha256::new();
        if let Some(len) = response.content_length() {
            if len != request.expected_size {
                return Err((
                    fail::SIZE_MISMATCH,
                    format!("server size {len} != expected {}", request.expected_size),
                ));
            }
        }
    }

    let mut file = open_part_file(part_path, resuming)
        .await
        .map_err(|e| (fail::IO, e.to_string()))?;

    emit(
        on_event,
        DownloadEvent::Started {
            total: request.expected_size,
            resumed_from: *received,
        },
    );

    // Stream chunks: write, hash, and report real byte counts. Progress events
    // are throttled to ~10/s so the IPC channel never floods, but the final
    // byte count is always emitted.
    let mut stream = response.bytes_stream();
    let mut last_emit = Instant::now();
    while let Some(chunk) = stream.next().await {
        if cancel.load(Ordering::Relaxed) {
            // Keep the partial file for resume; flush what we have.
            let _ = file.flush().await;
            emit(on_event, DownloadEvent::Canceled);
            return Ok(());
        }
        let chunk = chunk.map_err(|e| (fail::NETWORK, e.to_string()))?;
        *received += chunk.len() as u64;
        if *received > request.expected_size {
            // More bytes than the API published — not the asset we verified.
            drop(file);
            let _ = tokio::fs::remove_file(part_path).await;
            return Err((
                fail::SIZE_MISMATCH,
                format!(
                    "received {} > expected {}",
                    *received, request.expected_size
                ),
            ));
        }
        hasher.update(&chunk);
        file.write_all(&chunk)
            .await
            .map_err(|e| (fail::IO, e.to_string()))?;
        if last_emit.elapsed() >= Duration::from_millis(100) || *received == request.expected_size {
            emit(
                on_event,
                DownloadEvent::Progress {
                    received: *received,
                    total: request.expected_size,
                },
            );
            last_emit = Instant::now();
        }
    }

    if *received != request.expected_size {
        // Short download — keep the partial file so a retry resumes it.
        let _ = file.flush().await;
        return Err((
            fail::SHORT_DOWNLOAD,
            format!("received {} of {}", *received, request.expected_size),
        ));
    }

    // Make the fully-received bytes durable before verification reads them as
    // the source of truth.
    file.flush().await.map_err(|e| (fail::IO, e.to_string()))?;
    file.sync_all()
        .await
        .map_err(|e| (fail::IO, e.to_string()))?;
    Ok(())
}

/// Where installers land: a dedicated folder under the OS temp dir. Phase 5
/// executes verified installers from here.
///
/// Namespaced per host binary: the engine runs inside Freally Central AND
/// inside host apps embedding the panel (FC-50), as separate processes whose
/// in-process `Downloads` registries can't see each other — one shared dir
/// would let two processes interleave writes into the same `.part` file.
/// The same host keeps its dir across runs, so resume still works.
fn download_dir() -> PathBuf {
    let host = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.file_stem().map(|s| s.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "host".to_string());
    std::env::temp_dir()
        .join("freally-central-downloads")
        .join(host)
}

/// One shared HTTP client for every download: reqwest clients are built to be
/// shared, and reusing the pool lets Download All's transfers to the same two
/// hosts (github.com → its CDN) skip repeated TCP+TLS handshakes.
fn shared_client() -> Result<&'static reqwest::Client, Fail> {
    static CLIENT: std::sync::OnceLock<Result<reqwest::Client, String>> =
        std::sync::OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .user_agent(concat!("freally-central/", env!("CARGO_PKG_VERSION")))
                .redirect(https_only_redirects())
                .connect_timeout(Duration::from_secs(30))
                .read_timeout(Duration::from_secs(60))
                .build()
                .map_err(|e| e.to_string())
        })
        .as_ref()
        .map_err(|e| (fail::NETWORK, e.clone()))
}

/// Follow redirects only over https (GitHub bounces assets to
/// objects.githubusercontent.com; a downgrade to http would be refused).
fn https_only_redirects() -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(|attempt| {
        if attempt.previous().len() >= 10 {
            attempt.error("too many redirects")
        } else if attempt.url().scheme() != "https" {
            attempt.error("redirect left https")
        } else {
            attempt.follow()
        }
    })
}

/// Hash an existing partial file so a resumed download's digest still covers
/// every byte. Returns the byte count hashed (0 = nothing usable). A complete
/// .part (a cancel landed during verification) is kept and re-verified without
/// a network trip; only a LARGER-than-expected file is stale — discard it.
async fn hash_existing_prefix(
    part_path: &Path,
    hasher: &mut Sha256,
    expected_size: u64,
) -> std::io::Result<u64> {
    let Ok(metadata) = tokio::fs::metadata(part_path).await else {
        return Ok(0); // no partial file
    };
    if !metadata.is_file() || metadata.len() > expected_size {
        let _ = tokio::fs::remove_file(part_path).await;
        return Ok(0);
    }
    let mut file = tokio::fs::File::open(part_path).await?;
    hash_file_into(&mut file, hasher).await
}

/// Hash the remainder of an open file into `hasher`, returning the byte count.
/// Shared by resume-prefix hashing here and the install engine's execute-time
/// re-hash (Phase 5) — one buffered loop to keep correct.
pub(crate) async fn hash_file_into(
    file: &mut tokio::fs::File,
    hasher: &mut Sha256,
) -> std::io::Result<u64> {
    let mut buf = vec![0u8; 256 * 1024];
    let mut hashed: u64 = 0;
    loop {
        let n = file.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        hashed += n as u64;
    }
    Ok(hashed)
}

/// Open the partial file: append when resuming, truncate for a fresh start.
async fn open_part_file(part_path: &Path, resuming: bool) -> std::io::Result<tokio::fs::File> {
    let mut options = tokio::fs::OpenOptions::new();
    if resuming {
        options.append(true).create(true);
    } else {
        options.write(true).create(true).truncate(true);
    }
    options.open(part_path).await
}

/// Accept only the GitHub release-asset URL shape:
/// https://github.com/<owner>/<repo>/releases/download/<tag>/<file>
/// Shared with the install engine's trust gate (Phase 5), which re-validates
/// the recorded source URL with the same rules before anything executes.
pub(crate) fn validate_asset_url(raw: &str) -> Result<reqwest::Url, String> {
    let url = reqwest::Url::parse(raw).map_err(|e| format!("invalid url: {e}"))?;
    if url.scheme() != "https" {
        return Err(format!("scheme {} is not https", url.scheme()));
    }
    if url.host_str() != Some("github.com") {
        return Err(format!("host {:?} is not github.com", url.host_str()));
    }
    let segments: Vec<&str> = url
        .path_segments()
        .map(|s| s.filter(|p| !p.is_empty()).collect())
        .unwrap_or_default();
    let is_asset_path =
        segments.len() == 6 && segments[2] == "releases" && segments[3] == "download";
    if !is_asset_path {
        return Err(format!("path {} is not a release asset", url.path()));
    }
    Ok(url)
}

/// The asset name becomes an on-disk file name — allow only a plain name, no
/// separators, traversal, drive/stream colons, or hidden-file prefix.
fn safe_file_name(name: &str) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() || name.len() > 200 {
        return Err("file name empty or too long".into());
    }
    if name.starts_with('.') || name.contains("..") {
        return Err(format!("suspicious file name: {name}"));
    }
    if name
        .chars()
        .any(|c| matches!(c, '/' | '\\' | ':' | '\0') || c.is_control())
    {
        return Err(format!("illegal character in file name: {name}"));
    }
    Ok(name.to_string())
}

/// Parse the GitHub API's asset digest ("sha256:<64 hex>", raw hex accepted)
/// into lowercase hex. None = unusable, and verification can't be claimed.
fn parse_sha256(digest: &str) -> Option<String> {
    let hex_part = digest
        .trim()
        .strip_prefix("sha256:")
        .unwrap_or(digest.trim());
    let normalized = hex_part.to_ascii_lowercase();
    (normalized.len() == 64 && normalized.chars().all(|c| c.is_ascii_hexdigit()))
        .then_some(normalized)
}

/// The total size from a Content-Range header ("bytes 100-999/12345" → 12345).
fn total_from_content_range(value: &str) -> Option<u64> {
    let (unit_and_range, total) = value.trim().rsplit_once('/')?;
    if !unit_and_range.starts_with("bytes ") {
        return None;
    }
    total.trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_only_github_release_asset_urls() {
        assert!(validate_asset_url(
            "https://github.com/MikesRuthless12/freally-capture/releases/download/v1.4.0/Freally-Capture_1.4.0_x64-setup.exe"
        )
        .is_ok());
        // Wrong scheme, host, or path shape are all refused.
        assert!(validate_asset_url("http://github.com/o/r/releases/download/v1/file.exe").is_err());
        assert!(
            validate_asset_url("https://evil.example/o/r/releases/download/v1/file.exe").is_err()
        );
        assert!(validate_asset_url("https://github.com/o/r/archive/main.zip").is_err());
        assert!(
            validate_asset_url("https://github.com/o/r/releases/download/v1/extra/file.exe")
                .is_err()
        );
        assert!(validate_asset_url("not a url").is_err());
    }

    #[test]
    fn file_names_cannot_escape_the_download_dir() {
        assert_eq!(
            safe_file_name(" app_1.0.0_x64-setup.exe ").as_deref(),
            Ok("app_1.0.0_x64-setup.exe")
        );
        assert!(safe_file_name("../evil.exe").is_err());
        assert!(safe_file_name("a/b.exe").is_err());
        assert!(safe_file_name("a\\b.exe").is_err());
        assert!(safe_file_name("C:evil.exe").is_err());
        assert!(safe_file_name(".hidden").is_err());
        assert!(safe_file_name("").is_err());
        assert!(safe_file_name(&"x".repeat(201)).is_err());
    }

    #[test]
    fn parses_github_asset_digests() {
        let hex64 = "a".repeat(64);
        assert_eq!(
            parse_sha256(&format!("sha256:{hex64}")).as_deref(),
            Some(hex64.as_str())
        );
        // Raw hex and uppercase normalize; anything else is unusable.
        assert_eq!(
            parse_sha256(&"A".repeat(64)).as_deref(),
            Some(hex64.as_str())
        );
        assert_eq!(parse_sha256("sha512:abc"), None);
        assert_eq!(parse_sha256("sha256:tooshort"), None);
        assert_eq!(parse_sha256(""), None);
    }

    #[test]
    fn parses_content_range_totals() {
        assert_eq!(total_from_content_range("bytes 100-999/12345"), Some(12345));
        assert_eq!(total_from_content_range("bytes 0-0/1"), Some(1));
        // Unknown totals or non-byte units can't verify a resume.
        assert_eq!(total_from_content_range("bytes 100-999/*"), None);
        assert_eq!(total_from_content_range("items 1-2/3"), None);
        assert_eq!(total_from_content_range("garbage"), None);
    }
}
