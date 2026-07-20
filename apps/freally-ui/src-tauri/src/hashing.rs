//! Shared file-hash helper.
//!
//! `freally_hash::hash_file_async` streams progress on a bounded
//! channel; every caller that only wants the final hex digest has to
//! spawn a drain task so the sender never stalls. That six-line dance
//! was copy-pasted across the collision quick-hash, undo re-verify, and
//! checksum-sidecar paths — this is the one shared version.

use std::path::Path;

use freally_hash::HashAlgorithm;

/// Hash `path` with `algo` and return the lowercase hex digest. Drains
/// the progress channel concurrently so the hasher never blocks.
pub(crate) async fn hash_file_hex(path: &Path, algo: HashAlgorithm) -> Result<String, String> {
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let report = freally_hash::hash_file_async(path, algo, freally_core::CopyControl::new(), tx)
        .await
        .map_err(|e| e.to_string())?;
    let _ = drain.await;
    Ok(report.hex())
}
