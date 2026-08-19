//! Phase 50g — remote and cached [`BlobBackend`] implementations.
//!
//! 50g shipped the `BlobBackend` abstraction and `LocalFsBackend`; this
//! crate provides the two pieces its notes list as deferred: a concrete
//! object-store backend and a read-through cache.
//!
//! # Why a dedicated runtime
//!
//! `BlobBackend` is **synchronous** — the chunk store calls it from
//! plain functions, and the app already drives those through
//! `spawn_blocking`. OpenDAL is **async**. Bridging with
//! `Handle::block_on` would panic when a caller happens to be on a
//! runtime worker thread, so [`OpendalBackend`] owns a small
//! multi-threaded runtime and blocks on *that*. Blocking one thread of
//! a private runtime cannot deadlock the caller's, and it is the same
//! pattern `freally-server`'s OTel exporter uses for the same reason.
//!
//! # Why caching is safe here
//!
//! Pack objects are content-addressed and immutable once written: a
//! pack is appended to, sealed, and thereafter only ever deleted. A
//! cached byte range can therefore never go stale — the only
//! invalidation needed is on `delete`, which [`CachingBackend`] does.

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use freally_chunk::{BlobBackend, ChunkStoreError, Result};

/// Map an OpenDAL error onto the store's error type.
///
/// `NotFound` is deliberately *not* mapped here — every method that can
/// legitimately miss returns `Ok(None)` instead, matching
/// `LocalFsBackend`'s contract.
fn io_err(name: &str, e: opendal::Error) -> ChunkStoreError {
    ChunkStoreError::Io {
        path: PathBuf::from(name),
        source: std::io::Error::other(e.to_string()),
    }
}

/// A [`BlobBackend`] over any OpenDAL service — S3 and its
/// compatibles (R2, B2), Azure Blob, GCS, WebDAV, FTP, or a local
/// filesystem.
///
/// Built from a [`freally_cloud::Backend`] so a repository backend is
/// configured through the same surface as a cloud remote instead of a
/// second, divergent credential path.
pub struct OpendalBackend {
    op: opendal::Operator,
    rt: tokio::runtime::Runtime,
}

impl std::fmt::Debug for OpendalBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpendalBackend").finish_non_exhaustive()
    }
}

impl OpendalBackend {
    /// Build from a configured cloud backend and its secret blob.
    pub fn new(
        backend: &freally_cloud::Backend,
        secret: Option<&str>,
    ) -> std::result::Result<Self, String> {
        let op = freally_cloud::make_operator(backend, secret).map_err(|e| e.to_string())?;
        Self::from_operator(op)
    }

    /// Build directly from an operator. Mostly for tests, which point
    /// the `fs` service at a tempdir and exercise the whole contract
    /// with no network and no credentials.
    pub fn from_operator(op: opendal::Operator) -> std::result::Result<Self, String> {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .thread_name("freally-blob")
            .build()
            .map_err(|e| format!("blob backend runtime: {e}"))?;
        Ok(Self { op, rt })
    }
}

impl BlobBackend for OpendalBackend {
    fn get(&self, name: &str) -> Result<Option<Vec<u8>>> {
        self.rt.block_on(async {
            match self.op.read(name).await {
                Ok(buf) => Ok(Some(buf.to_vec())),
                Err(e) if e.kind() == opendal::ErrorKind::NotFound => Ok(None),
                Err(e) => Err(io_err(name, e)),
            }
        })
    }

    /// Range read — the whole point of using an object store rather
    /// than pulling whole packs.
    ///
    /// The range is clamped to the object's real length first. OpenDAL
    /// treats a range that runs past EOF as an error ("reader got too
    /// little data"), whereas the trait's own default — and
    /// `LocalFsBackend` — clamp and return the short tail. Without this
    /// the two backends would disagree on every read near the end of a
    /// pack, which is exactly where chunk reads land.
    fn get_range(&self, name: &str, offset: u64, len: usize) -> Result<Option<Vec<u8>>> {
        self.rt.block_on(async {
            let total = match self.op.stat(name).await {
                Ok(meta) => meta.content_length(),
                Err(e) if e.kind() == opendal::ErrorKind::NotFound => return Ok(None),
                Err(e) => return Err(io_err(name, e)),
            };
            let start = offset.min(total);
            let end = start.saturating_add(len as u64).min(total);
            if start >= end {
                // A zero-length window is a legitimate request, not an
                // error; the object exists, there is just nothing there.
                return Ok(Some(Vec::new()));
            }
            match self.op.read_with(name).range(start..end).await {
                Ok(buf) => Ok(Some(buf.to_vec())),
                Err(e) if e.kind() == opendal::ErrorKind::NotFound => Ok(None),
                Err(e) => Err(io_err(name, e)),
            }
        })
    }

    fn put(&self, name: &str, bytes: &[u8]) -> Result<()> {
        self.rt.block_on(async {
            self.op
                .write(name, bytes.to_vec())
                .await
                .map_err(|e| io_err(name, e))
                .map(|_| ())
        })
    }

    fn delete(&self, name: &str) -> Result<u64> {
        self.rt.block_on(async {
            // Report the freed size, so store accounting stays honest.
            let freed = match self.op.stat(name).await {
                Ok(meta) => meta.content_length(),
                Err(e) if e.kind() == opendal::ErrorKind::NotFound => return Ok(0),
                Err(e) => return Err(io_err(name, e)),
            };
            self.op.delete(name).await.map_err(|e| io_err(name, e))?;
            Ok(freed)
        })
    }

    fn list(&self, prefix: &str) -> Result<Vec<String>> {
        self.rt.block_on(async {
            let entries = self.op.list(prefix).await.map_err(|e| io_err(prefix, e))?;
            Ok(entries
                .into_iter()
                .filter(|e| e.metadata().is_file())
                .map(|e| e.path().to_string())
                .collect())
        })
    }

    fn exists(&self, name: &str) -> Result<bool> {
        self.rt.block_on(async {
            match self.op.stat(name).await {
                Ok(_) => Ok(true),
                Err(e) if e.kind() == opendal::ErrorKind::NotFound => Ok(false),
                Err(e) => Err(io_err(name, e)),
            }
        })
    }

    /// HEAD rather than a full read — the reason the trait leaves this
    /// overridable.
    fn size(&self, name: &str) -> Result<Option<u64>> {
        self.rt.block_on(async {
            match self.op.stat(name).await {
                Ok(meta) => Ok(Some(meta.content_length())),
                Err(e) if e.kind() == opendal::ErrorKind::NotFound => Ok(None),
                Err(e) => Err(io_err(name, e)),
            }
        })
    }
}

/// A read-through disk cache in front of any [`BlobBackend`].
///
/// Safe because pack objects are immutable once sealed (see the module
/// docs): a cached object can never be stale, only absent. `put` and
/// `delete` keep the cache coherent for the one mutable case.
///
/// Range reads intentionally do **not** populate the cache: caching a
/// slice under the object's name would make a later whole-object `get`
/// return a truncated body. A range read consults a fully cached
/// object and otherwise goes straight to the inner backend.
#[derive(Debug)]
pub struct CachingBackend<B: BlobBackend> {
    inner: B,
    dir: PathBuf,
}

impl<B: BlobBackend> CachingBackend<B> {
    /// Wrap `inner`, caching object bodies under `dir`.
    pub fn new(inner: B, dir: impl Into<PathBuf>) -> Self {
        Self {
            inner,
            dir: dir.into(),
        }
    }

    /// Cache path for `name`.
    ///
    /// Object names contain `/`, so they are flattened — a nested path
    /// would otherwise collide with a directory the cache also wants to
    /// create.
    fn path_for(&self, name: &str) -> PathBuf {
        self.dir.join(name.replace(['/', '\\'], "_"))
    }

    fn read_cached(&self, name: &str) -> Option<Vec<u8>> {
        std::fs::read(self.path_for(name)).ok()
    }

    /// Read one slice out of a cached object without materialising the
    /// whole body.
    ///
    /// Packs roll over at `PACK_ROLLOVER_BYTES` (1 GiB) and chunks
    /// average ~1 MiB, so serving a range by reading the entire cached
    /// pack and slicing would move roughly a thousand times more bytes
    /// than the caller asked for. Seeking costs one `open` + one
    /// `read_exact` of exactly `len`.
    ///
    /// Returns `None` when the object is not cached (the caller then
    /// falls through to the inner backend) and a short — possibly
    /// empty — buffer when the requested window runs past the end,
    /// matching the clamping the whole-body path used to do.
    fn read_cached_range(&self, name: &str, offset: u64, len: usize) -> Option<Vec<u8>> {
        let mut f = std::fs::File::open(self.path_for(name)).ok()?;
        let size = f.metadata().ok()?.len();
        let start = offset.min(size);
        let end = start.saturating_add(len as u64).min(size);
        let want = (end - start) as usize;
        if want == 0 {
            return Some(Vec::new());
        }
        f.seek(SeekFrom::Start(start)).ok()?;
        let mut buf = vec![0u8; want];
        f.read_exact(&mut buf).ok()?;
        Some(buf)
    }

    /// Write via a temp file + rename so an interrupted write can never
    /// leave a truncated body that later reads would trust.
    fn write_cached(&self, name: &str, bytes: &[u8]) {
        if std::fs::create_dir_all(&self.dir).is_err() {
            return;
        }
        let final_path = self.path_for(name);
        let tmp = final_path.with_extension("partial");
        let ok = std::fs::File::create(&tmp)
            .and_then(|mut f| f.write_all(bytes).map(|_| f))
            .and_then(|f| f.sync_all())
            .is_ok();
        if ok {
            let _ = std::fs::rename(&tmp, &final_path);
        } else {
            let _ = std::fs::remove_file(&tmp);
        }
    }

    fn evict(&self, name: &str) {
        let _ = std::fs::remove_file(self.path_for(name));
    }

    /// The wrapped backend.
    pub fn inner(&self) -> &B {
        &self.inner
    }
}

impl<B: BlobBackend> BlobBackend for CachingBackend<B> {
    fn get(&self, name: &str) -> Result<Option<Vec<u8>>> {
        if let Some(hit) = self.read_cached(name) {
            return Ok(Some(hit));
        }
        let fetched = self.inner.get(name)?;
        if let Some(bytes) = &fetched {
            self.write_cached(name, bytes);
        }
        Ok(fetched)
    }

    fn get_range(&self, name: &str, offset: u64, len: usize) -> Result<Option<Vec<u8>>> {
        if let Some(hit) = self.read_cached_range(name, offset, len) {
            return Ok(Some(hit));
        }
        self.inner.get_range(name, offset, len)
    }

    fn put(&self, name: &str, bytes: &[u8]) -> Result<()> {
        self.inner.put(name, bytes)?;
        self.write_cached(name, bytes);
        Ok(())
    }

    fn delete(&self, name: &str) -> Result<u64> {
        let freed = self.inner.delete(name)?;
        self.evict(name);
        Ok(freed)
    }

    fn list(&self, prefix: &str) -> Result<Vec<String>> {
        self.inner.list(prefix)
    }

    fn exists(&self, name: &str) -> Result<bool> {
        if self.path_for(name).is_file() {
            return Ok(true);
        }
        self.inner.exists(name)
    }

    fn size(&self, name: &str) -> Result<Option<u64>> {
        if let Ok(meta) = std::fs::metadata(self.path_for(name)) {
            return Ok(Some(meta.len()));
        }
        self.inner.size(name)
    }
}

/// Build an `fs`-service operator rooted at `root`.
///
/// Exposed because it is how the tests exercise the real OpenDAL code
/// path with no network, and it is a legitimate deployment too — an
/// NFS/SMB mount is an object store as far as this backend cares.
pub fn fs_operator(root: &Path) -> std::result::Result<opendal::Operator, String> {
    let builder = opendal::services::Fs::default().root(&root.to_string_lossy());
    Ok(opendal::Operator::new(builder)
        .map_err(|e| e.to_string())?
        .finish())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn backend(root: &Path) -> OpendalBackend {
        OpendalBackend::from_operator(fs_operator(root).unwrap()).unwrap()
    }

    #[test]
    fn opendal_backend_round_trips_the_whole_contract() {
        let dir = tempfile::tempdir().unwrap();
        let b = backend(dir.path());

        assert_eq!(b.get("missing").unwrap(), None);
        assert!(!b.exists("missing").unwrap());
        assert_eq!(b.size("missing").unwrap(), None);
        // Deleting an absent object is idempotent, not an error.
        assert_eq!(b.delete("missing").unwrap(), 0);

        b.put("pack-1", b"hello world").unwrap();
        assert!(b.exists("pack-1").unwrap());
        assert_eq!(b.get("pack-1").unwrap().unwrap(), b"hello world");
        assert_eq!(b.size("pack-1").unwrap(), Some(11));

        // Range read is the reason this backend exists.
        assert_eq!(b.get_range("pack-1", 6, 5).unwrap().unwrap(), b"world");
        // A range past the end clamps rather than erroring — OpenDAL
        // itself refuses this, so the backend must clamp first or it
        // would disagree with LocalFsBackend on every tail read.
        assert_eq!(b.get_range("pack-1", 6, 999).unwrap().unwrap(), b"world");
        // Wholly past the end is an empty window, not a failure.
        assert_eq!(b.get_range("pack-1", 99, 5).unwrap().unwrap(), b"");
        // A range on a missing object is still None, not empty.
        assert_eq!(b.get_range("missing", 0, 5).unwrap(), None);

        b.put("pack-2", b"second").unwrap();
        let mut listed = b.list("").unwrap();
        listed.sort();
        assert_eq!(listed, vec!["pack-1".to_string(), "pack-2".to_string()]);

        assert_eq!(b.delete("pack-1").unwrap(), 11);
        assert!(!b.exists("pack-1").unwrap());
    }

    #[test]
    fn opendal_overwrite_replaces_the_body() {
        let dir = tempfile::tempdir().unwrap();
        let b = backend(dir.path());
        b.put("p", b"aaaa").unwrap();
        b.put("p", b"bb").unwrap();
        assert_eq!(b.get("p").unwrap().unwrap(), b"bb");
        assert_eq!(b.size("p").unwrap(), Some(2));
    }

    /// Counts reads so the cache can be proven to actually avoid them.
    #[derive(Debug, Default)]
    struct CountingBackend {
        objects: Mutex<std::collections::HashMap<String, Vec<u8>>>,
        gets: AtomicUsize,
    }

    impl BlobBackend for CountingBackend {
        fn get(&self, name: &str) -> Result<Option<Vec<u8>>> {
            self.gets.fetch_add(1, Ordering::SeqCst);
            Ok(self.objects.lock().unwrap().get(name).cloned())
        }
        fn put(&self, name: &str, bytes: &[u8]) -> Result<()> {
            self.objects
                .lock()
                .unwrap()
                .insert(name.to_string(), bytes.to_vec());
            Ok(())
        }
        fn delete(&self, name: &str) -> Result<u64> {
            Ok(self
                .objects
                .lock()
                .unwrap()
                .remove(name)
                .map(|v| v.len() as u64)
                .unwrap_or(0))
        }
        fn list(&self, prefix: &str) -> Result<Vec<String>> {
            Ok(self
                .objects
                .lock()
                .unwrap()
                .keys()
                .filter(|k| k.starts_with(prefix))
                .cloned()
                .collect())
        }
        fn exists(&self, name: &str) -> Result<bool> {
            Ok(self.objects.lock().unwrap().contains_key(name))
        }
    }

    #[test]
    fn cache_serves_the_second_read_without_touching_the_backend() {
        let dir = tempfile::tempdir().unwrap();
        let c = CachingBackend::new(CountingBackend::default(), dir.path());
        c.put("p", b"payload").unwrap();

        // `put` populates, so even the first read is a hit.
        assert_eq!(c.get("p").unwrap().unwrap(), b"payload");
        assert_eq!(c.get("p").unwrap().unwrap(), b"payload");
        assert_eq!(
            c.inner().gets.load(Ordering::SeqCst),
            0,
            "cached reads must not reach the remote"
        );
    }

    #[test]
    fn cold_cache_fetches_once_then_serves_locally() {
        let dir = tempfile::tempdir().unwrap();
        let inner = CountingBackend::default();
        inner.put("p", b"payload").unwrap();
        let c = CachingBackend::new(inner, dir.path());

        assert_eq!(c.get("p").unwrap().unwrap(), b"payload");
        assert_eq!(c.get("p").unwrap().unwrap(), b"payload");
        assert_eq!(
            c.inner().gets.load(Ordering::SeqCst),
            1,
            "a cold miss should fetch exactly once"
        );
    }

    #[test]
    fn delete_evicts_so_a_later_read_cannot_resurrect_it() {
        let dir = tempfile::tempdir().unwrap();
        let c = CachingBackend::new(CountingBackend::default(), dir.path());
        c.put("p", b"payload").unwrap();
        assert_eq!(c.delete("p").unwrap(), 7);
        assert_eq!(
            c.get("p").unwrap(),
            None,
            "a deleted object must not survive in the cache"
        );
        assert!(!c.exists("p").unwrap());
    }

    #[test]
    fn range_read_is_not_cached_as_a_whole_object() {
        // The bug this guards: caching a slice under the object's name
        // would make the next whole-object get return a truncated body.
        let dir = tempfile::tempdir().unwrap();
        let inner = CountingBackend::default();
        inner.put("p", b"0123456789").unwrap();
        let c = CachingBackend::new(inner, dir.path());

        assert_eq!(c.get_range("p", 2, 3).unwrap().unwrap(), b"234");
        assert_eq!(c.get("p").unwrap().unwrap(), b"0123456789");
    }

    #[test]
    fn caching_over_opendal_composes() {
        let store = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let c = CachingBackend::new(backend(store.path()), cache.path());

        c.put("pack-1", b"hello world").unwrap();
        assert_eq!(c.get("pack-1").unwrap().unwrap(), b"hello world");
        assert_eq!(c.get_range("pack-1", 0, 5).unwrap().unwrap(), b"hello");
        assert_eq!(c.size("pack-1").unwrap(), Some(11));
        assert_eq!(c.delete("pack-1").unwrap(), 11);
        assert_eq!(c.get("pack-1").unwrap(), None);
    }
}
