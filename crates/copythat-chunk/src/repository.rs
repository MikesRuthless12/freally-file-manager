//! Phase 49 (moonshot) — the unified sync + backup repository.
//!
//! Phase 27 gave us a content-defined [`ChunkStore`](crate::ChunkStore):
//! every distinct chunk is stored once, keyed by its BLAKE3 digest.
//! Phase 49 builds *one repository concept* on top of it so that a
//! copy job, a sync run, and a versioning checkpoint all land in the
//! same content-addressed store and become queryable from a single
//! timeline — "every byte stored once; every past state queryable from
//! the same view".
//!
//! # What this module is (and is not)
//!
//! [`Repository`] **owns** a [`ChunkStore`] as a sub-component and adds
//! a snapshot catalog in a sibling redb file (`<root>/repository.redb`):
//!
//! - a **snapshot catalog** ([`SNAPSHOTS`]) — the full [`Snapshot`]s,
//!   each a set of [`FileEntry`]s (logical path + its [`Manifest`])
//!   tagged with a [`SnapshotKind`] and a millisecond timestamp; and
//! - a **summaries index** ([`SUMMARIES`]) — one lightweight
//!   [`UnifiedSnapshot`] per snapshot so the timeline listing never
//!   deserializes a single chunk manifest.
//!
//! Chunk lifetime is managed by **reference counting at GC time**:
//! [`Repository::gc`] walks every manifest source — the snapshots *and*
//! the [`ChunkStore`]'s own Phase 27 `manifests` table — to compute the
//! reachable set, then sweeps everything else. (An earlier draft kept a
//! persisted `chunks_refcount` table, but GC necessarily recomputes
//! reachability from the manifests anyway, so the table was redundant
//! state that could only drift; [`Repository::chunk_refcount`] now
//! derives the count on demand.)
//!
//! Deliberately, this layer is **self-contained**: it pulls in no new
//! dependencies and stays synchronous + `#![forbid(unsafe_code)]` like
//! the rest of `copythat-chunk`. It does *not* embed the async
//! `copythat-history` / `copythat-sync` / `copythat-journal` databases
//! or the platform-`unsafe` `copythat-audit` sink — instead it is the
//! shared content + timeline layer those engines record *into*.
//!
//! # Timestamps
//!
//! Callers pass `created_at_ms: i64` — milliseconds since the Unix epoch
//! — matching the `*_ms` convention already used across
//! `copythat-history`, `-sync`, and `-journal`. The caller owns the
//! clock; the repository only ever orders by the value it was given.
//!
//! # Concurrency
//!
//! All mutating entry points take a shared *read* lease on an internal
//! [`RwLock`]; [`Repository::gc`] takes the exclusive *write* lease.
//! Recording snapshots can therefore run concurrently with each other,
//! but GC can never observe a half-written snapshot (chunks committed to
//! the store but whose snapshot row has not yet landed) — which would
//! otherwise let GC sweep live chunks.
//!
//! # Crash consistency
//!
//! Recording a snapshot writes chunks to the [`ChunkStore`] first (each
//! `put` is its own ACID redb transaction), then commits the snapshot
//! row + summary in a single `repository.redb` transaction. A crash
//! *between* the two leaves chunks that no snapshot references —
//! harmless orphans that the next [`Repository::gc`] reclaims. There is
//! no window in which a committed snapshot points at a missing chunk.
//!
//! # Example
//!
//! ```no_run
//! use copythat_chunk::{Repository, SnapshotKind};
//!
//! let repo = Repository::open(std::path::Path::new("/tmp/repo"))?;
//! let id = repo.snapshot_bytes(
//!     SnapshotKind::Copy,
//!     "nightly copy of /docs",
//!     1_700_000_000_000,
//!     &[("/docs/report.pdf", b"...file bytes...")],
//! )?;
//! println!("recorded snapshot {}", id.as_u64());
//! for s in repo.snapshots()? {
//!     println!("{:?} {} ({} files)", s.kind, s.label, s.file_count);
//! }
//! let report = repo.gc()?;
//! println!("gc swept {} orphan chunks", report.chunks_swept);
//! # Ok::<(), copythat_chunk::ChunkStoreError>(())
//! ```

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::sync::RwLock;

use redb::{Database, ReadableTable, ReadableTableMetadata, TableDefinition};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::chunker::Chunker;
use crate::error::{ChunkStoreError, Result};
use crate::manifest::{chunk_into_store, materialise_file};
use crate::store::{ChunkStore, default_chunk_store_path};
use crate::types::{Blake3Hash, ChunkRef, Manifest};

/// Snapshot catalog: snapshot id (`u64`) → JSON-serialised [`Snapshot`].
const SNAPSHOTS: TableDefinition<'_, u64, &[u8]> = TableDefinition::new("snapshots");

/// Lightweight per-snapshot summary ([`UnifiedSnapshot`]) so the
/// timeline listing never touches chunk manifests.
const SUMMARIES: TableDefinition<'_, u64, &[u8]> = TableDefinition::new("snapshot_summaries");

/// Monotonic-id allocator (`"next-snapshot-id"` → next free `u64`).
const SEQ: TableDefinition<'_, &str, u64> = TableDefinition::new("seq");

/// Key under [`SEQ`] holding the next snapshot id to hand out.
const SEQ_NEXT_SNAPSHOT_ID: &str = "next-snapshot-id";

/// What produced a snapshot. Unifies the three previously-separate
/// histories (Phase 9 copy log, Phase 25 sync state, Phase 42 version
/// checkpoints) plus an explicit backup snapshot, into one timeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SnapshotKind {
    /// A copy/move job (Phase 1–9).
    Copy,
    /// A two-way sync round (Phase 25).
    Sync,
    /// A version checkpoint of an overwritten/watched file (Phase 42).
    Version,
    /// An explicit, user-triggered backup snapshot of a folder.
    Backup,
}

impl SnapshotKind {
    /// Stable lowercase tag (matches the on-disk kebab-case form).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Copy => "copy",
            Self::Sync => "sync",
            Self::Version => "version",
            Self::Backup => "backup",
        }
    }
}

/// Opaque identifier for a recorded [`Snapshot`]. Monotonic, starts at 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SnapshotId(pub u64);

impl SnapshotId {
    /// The raw `u64`.
    #[must_use]
    pub const fn as_u64(self) -> u64 {
        self.0
    }
}

impl std::fmt::Display for SnapshotId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// One file inside a [`Snapshot`]: a logical path plus the [`Manifest`]
/// describing the chunks that reconstruct it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileEntry {
    /// Logical path this entry was captured under. Opaque to the
    /// repository — callers typically use the source or destination
    /// absolute path.
    pub path: String,
    /// Chunk manifest (Phase 27). Concatenating its chunks'
    /// bytes reconstructs the file.
    pub manifest: Manifest,
}

/// A recorded point-in-time state of one or more files.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    /// Monotonic id.
    pub id: u64,
    /// What produced this snapshot.
    pub kind: SnapshotKind,
    /// Caller-supplied capture time, milliseconds since the Unix epoch.
    pub created_at_ms: i64,
    /// Human-readable label (job description, source root, …).
    pub label: String,
    /// The files captured in this snapshot.
    pub files: Vec<FileEntry>,
}

impl Snapshot {
    /// The distinct chunk hashes this snapshot references.
    fn distinct_chunks(&self) -> HashSet<Blake3Hash> {
        let mut set = HashSet::new();
        for f in &self.files {
            for c in &f.manifest.chunks {
                set.insert(c.hash);
            }
        }
        set
    }

    /// Sum of the logical sizes of every file in this snapshot.
    fn total_size(&self) -> u64 {
        self.files.iter().map(|f| f.manifest.size).sum()
    }

    /// Build the lightweight summary persisted alongside the snapshot.
    fn summary(&self) -> UnifiedSnapshot {
        UnifiedSnapshot {
            id: self.id,
            kind: self.kind,
            created_at_ms: self.created_at_ms,
            label: self.label.clone(),
            file_count: self.files.len() as u64,
            total_size: self.total_size(),
        }
    }
}

/// Lightweight summary of a [`Snapshot`] for timeline listings — no
/// manifests, persisted in its own index so the whole catalog is cheap
/// to return.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnifiedSnapshot {
    /// Snapshot id.
    pub id: u64,
    /// What produced it.
    pub kind: SnapshotKind,
    /// Capture time, ms since epoch.
    pub created_at_ms: i64,
    /// Human-readable label.
    pub label: String,
    /// Number of files captured.
    pub file_count: u64,
    /// Sum of logical file sizes (the effective bytes this snapshot
    /// represents, before dedup).
    pub total_size: u64,
}

/// A single file resolved out of the timeline by [`Repository::snapshot_at`]
/// — carries the manifest so it can be handed straight to
/// [`Repository::restore`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileSnapshot {
    /// Logical path.
    pub path: String,
    /// The snapshot this version came from.
    pub snapshot_id: u64,
    /// What produced that snapshot.
    pub kind: SnapshotKind,
    /// When that snapshot was captured, ms since epoch.
    pub created_at_ms: i64,
    /// Chunk manifest for restoring the file.
    pub manifest: Manifest,
}

/// Aggregate numbers for the repository — drives the "💾 X stored,
/// serving Y effective (Z% saved)" hero readout.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct RepoStats {
    /// Physical bytes on disk across all pack files (includes not-yet-
    /// reclaimed dead space, so this is "disk used", not "live data").
    pub stored_bytes: u64,
    /// Sum of the lengths of every *distinct* chunk referenced by a
    /// snapshot — the deduplicated logical size of the live content,
    /// independent of pack overhead. Always `<= effective_bytes`.
    pub unique_bytes: u64,
    /// Sum of logical file sizes across every snapshot (what the store
    /// would cost without dedup + history sharing).
    pub effective_bytes: u64,
    /// Number of snapshots in the catalog.
    pub snapshot_count: u64,
    /// Number of distinct chunks currently indexed.
    pub chunk_count: u64,
}

impl RepoStats {
    /// Fraction of effective bytes saved by deduplication, in `0.0..=1.0`.
    ///
    /// Computed from [`Self::unique_bytes`] (the live deduplicated
    /// content size), **not** from [`Self::stored_bytes`] — physical
    /// pack files can carry not-yet-reclaimed dead space that would
    /// otherwise make a working dedup look like "0% saved". Returns
    /// `0.0` for an empty repository.
    #[must_use]
    pub fn saved_ratio(&self) -> f64 {
        if self.effective_bytes == 0 {
            return 0.0;
        }
        // unique_bytes <= effective_bytes by construction; min() is
        // belt-and-suspenders so the ratio can never go negative.
        let unique = self.unique_bytes.min(self.effective_bytes) as f64;
        1.0 - (unique / self.effective_bytes as f64)
    }
}

/// Outcome of a [`Repository::gc`] pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct GcReport {
    /// Chunks whose index entry was removed because no manifest source
    /// references them any more.
    pub chunks_swept: u64,
    /// Pack files deleted because every chunk they held was swept.
    pub packs_removed: u64,
    /// Disk bytes reclaimed by deleting those whole pack files.
    ///
    /// Note: only *fully*-dead, non-active pack files are deleted.
    /// Reclaiming the dead bytes inside a partially-live pack requires
    /// rewriting its survivors into a fresh pack — that compaction is a
    /// deferred follow-up (see the module docs), so a GC that sweeps
    /// index entries out of the still-live active pack can legitimately
    /// report `chunks_swept > 0` with `bytes_reclaimed == 0`.
    pub bytes_reclaimed: u64,
}

/// The Phase 49 unified repository: a [`ChunkStore`] plus a snapshot
/// catalog and reference-counted garbage collection.
pub struct Repository {
    store: ChunkStore,
    db: Database,
    chunker: Chunker,
    root: PathBuf,
    /// Mutators take a read lease; [`Repository::gc`] takes the write
    /// lease so it can never run while a snapshot is mid-record.
    gc_lock: RwLock<()>,
}

impl std::fmt::Debug for Repository {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Repository")
            .field("root", &self.root)
            .finish()
    }
}

impl Repository {
    /// Open (or create) a repository rooted at `root`.
    ///
    /// Lays out `<root>/index.redb` plus `<root>/packs/` (the
    /// [`ChunkStore`] sub-component) and `<root>/repository.redb` (the
    /// snapshot catalog). Both redb files are independent and each takes
    /// its own file lock.
    pub fn open(root: &Path) -> Result<Self> {
        let store = ChunkStore::open(root)?;
        let db_path = root.join("repository.redb");
        let db = Database::create(&db_path)?;
        // Materialise the catalog tables on first open.
        {
            let txn = db.begin_write()?;
            let _ = txn.open_table(SNAPSHOTS)?;
            let _ = txn.open_table(SUMMARIES)?;
            let _ = txn.open_table(SEQ)?;
            txn.commit()?;
        }
        Ok(Self {
            store,
            db,
            chunker: Chunker::default(),
            root: root.to_path_buf(),
            gc_lock: RwLock::new(()),
        })
    }

    /// Open the repository at the default user-data location
    /// (`<data-dir>/chunks`, the same root Phase 27 used for the chunk
    /// store).
    ///
    /// # Sharing the default store
    ///
    /// This opens `index.redb` at the default chunk-store path, which
    /// other features (mount, recovery) may already hold open via
    /// [`ChunkStore::open`]. redb takes an exclusive file lock, so a
    /// second concurrent opener fails. Until Phase 49 is wired into the
    /// app with a single shared store handle, treat `open_default()` as
    /// exclusive with those features.
    pub fn open_default() -> Result<Self> {
        Self::open(&default_chunk_store_path()?)
    }

    /// The repository root (the value passed to [`Self::open`]).
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Borrow the underlying [`ChunkStore`] sub-component — for the
    /// engine's delta-resume path, disk-usage readouts, and tests.
    #[must_use]
    pub fn store(&self) -> &ChunkStore {
        &self.store
    }

    // ----- recording -------------------------------------------------

    /// Chunk each `(path, bytes)`, store the chunks (dedup-aware), and
    /// record a [`Snapshot`] of kind `kind`. Returns the new id.
    ///
    /// In-memory entry point for callers that already hold the bytes;
    /// [`Self::snapshot_files`] streams from disk instead.
    pub fn snapshot_bytes(
        &self,
        kind: SnapshotKind,
        label: &str,
        created_at_ms: i64,
        files: &[(&str, &[u8])],
    ) -> Result<SnapshotId> {
        let _lease = self.read_lease()?;
        let mut entries = Vec::with_capacity(files.len());
        for (path, bytes) in files {
            let manifest = chunk_into_store(&self.store, &self.chunker, bytes)?.1;
            entries.push(FileEntry {
                path: (*path).to_string(),
                manifest,
            });
        }
        self.record_inner(kind, label, created_at_ms, entries)
    }

    /// Like [`Self::snapshot_bytes`] but **streams** each file from disk
    /// through the rolling chunker, so a multi-gigabyte file never has
    /// to be held in memory at once.
    pub fn snapshot_files(
        &self,
        kind: SnapshotKind,
        label: &str,
        created_at_ms: i64,
        files: &[(&str, &Path)],
    ) -> Result<SnapshotId> {
        let _lease = self.read_lease()?;
        let mut entries = Vec::with_capacity(files.len());
        for (path, src) in files {
            let file = File::open(src).map_err(|e| ChunkStoreError::Io {
                path: src.to_path_buf(),
                source: e,
            })?;
            let manifest = self.ingest_reader(BufReader::new(file))?;
            entries.push(FileEntry {
                path: (*path).to_string(),
                manifest,
            });
        }
        self.record_inner(kind, label, created_at_ms, entries)
    }

    /// Record a snapshot from already-built [`FileEntry`]s (the engine
    /// path — the copy/sync engines build manifests as they run).
    pub fn record(
        &self,
        kind: SnapshotKind,
        label: &str,
        created_at_ms: i64,
        files: Vec<FileEntry>,
    ) -> Result<SnapshotId> {
        let _lease = self.read_lease()?;
        self.record_inner(kind, label, created_at_ms, files)
    }

    /// Remove a snapshot from the catalog. Chunk bytes are **not** freed
    /// here — run [`Self::gc`] to reclaim chunks no manifest source
    /// references any more. Returns `false` if no such snapshot existed.
    pub fn remove_snapshot(&self, id: SnapshotId) -> Result<bool> {
        let _lease = self.read_lease()?;
        let txn = self.db.begin_write()?;
        let existed;
        {
            let mut snaps = txn.open_table(SNAPSHOTS)?;
            existed = snaps.remove(id.0)?.is_some();
            let mut summaries = txn.open_table(SUMMARIES)?;
            summaries.remove(id.0)?;
        }
        txn.commit()?;
        Ok(existed)
    }

    /// Stream a reader through the chunker into the store, returning the
    /// file's [`Manifest`]. Never buffers the whole input.
    fn ingest_reader<R: Read>(&self, reader: R) -> Result<Manifest> {
        let mut chunks = Vec::new();
        let mut offset = 0u64;
        // `chunk_reader` yields window-relative offsets; we track the
        // absolute offset ourselves and use the full-file hash it
        // returns so there's no extra pass over the bytes.
        let file_hash = self.chunker.chunk_reader(reader, |chunk, slice| {
            self.store.put(chunk.hash, slice)?;
            chunks.push(ChunkRef {
                hash: chunk.hash,
                offset,
                len: chunk.len,
            });
            offset += u64::from(chunk.len);
            Ok(())
        })?;
        Ok(Manifest {
            file_hash,
            size: offset,
            chunks,
        })
    }

    /// Persist a snapshot row + its summary in one transaction. Callers
    /// must already hold the read lease.
    fn record_inner(
        &self,
        kind: SnapshotKind,
        label: &str,
        created_at_ms: i64,
        files: Vec<FileEntry>,
    ) -> Result<SnapshotId> {
        let id = self.allocate_snapshot_id()?;
        let snap = Snapshot {
            id,
            kind,
            created_at_ms,
            label: label.to_string(),
            files,
        };
        let snap_bytes = serde_json::to_vec(&snap)?;
        let summary_bytes = serde_json::to_vec(&snap.summary())?;

        let txn = self.db.begin_write()?;
        {
            let mut snaps = txn.open_table(SNAPSHOTS)?;
            snaps.insert(id, snap_bytes.as_slice())?;
            let mut summaries = txn.open_table(SUMMARIES)?;
            summaries.insert(id, summary_bytes.as_slice())?;
        }
        txn.commit()?;
        Ok(SnapshotId(id))
    }

    // ----- querying --------------------------------------------------

    /// Fetch a full snapshot by id (manifests included).
    pub fn snapshot(&self, id: SnapshotId) -> Result<Option<Snapshot>> {
        let txn = self.db.begin_read()?;
        let snaps = txn.open_table(SNAPSHOTS)?;
        let Some(v) = snaps.get(id.0)? else {
            return Ok(None);
        };
        Ok(Some(serde_json::from_slice(v.value())?))
    }

    /// The whole timeline as lightweight summaries, oldest first
    /// (ordered by `created_at_ms`, then id). Reads only the summaries
    /// index, so it never deserializes a chunk manifest.
    pub fn snapshots(&self) -> Result<Vec<UnifiedSnapshot>> {
        let txn = self.db.begin_read()?;
        let summaries = txn.open_table(SUMMARIES)?;
        let mut out = Vec::with_capacity(summaries.len()? as usize);
        for row in summaries.iter()? {
            let (_k, v) = row?;
            out.push(serde_json::from_slice::<UnifiedSnapshot>(v.value())?);
        }
        out.sort_by_key(|s| (s.created_at_ms, s.id));
        Ok(out)
    }

    /// Resolve the state of `path` as of `ts_ms`: the newest snapshot at
    /// or before `ts_ms` that contains `path`. Returns `None` if the
    /// file had not yet been captured by then.
    pub fn snapshot_at(&self, path: &str, ts_ms: i64) -> Result<Option<FileSnapshot>> {
        let mut best: Option<FileSnapshot> = None;
        for snap in self.load_all_snapshots()? {
            if snap.created_at_ms > ts_ms {
                continue;
            }
            let Some(entry) = snap.files.iter().find(|f| f.path == path) else {
                continue;
            };
            let newer = match &best {
                None => true,
                Some(b) => (snap.created_at_ms, snap.id) > (b.created_at_ms, b.snapshot_id),
            };
            if newer {
                best = Some(FileSnapshot {
                    path: entry.path.clone(),
                    snapshot_id: snap.id,
                    kind: snap.kind,
                    created_at_ms: snap.created_at_ms,
                    manifest: entry.manifest.clone(),
                });
            }
        }
        Ok(best)
    }

    /// Materialise a resolved [`FileSnapshot`] back to `dst`, pulling
    /// each chunk from the store. Errors with
    /// [`ChunkStoreError::MissingChunk`] if a referenced chunk has been
    /// garbage-collected.
    pub fn restore(&self, snapshot: &FileSnapshot, dst: &Path) -> Result<()> {
        materialise_file(&self.store, &snapshot.manifest, dst)
    }

    /// Aggregate stats for the hero readout.
    pub fn stats(&self) -> Result<RepoStats> {
        let snaps = self.load_all_snapshots()?;
        let effective_bytes = snaps.iter().map(Snapshot::total_size).sum();
        // Deduplicated logical size: each distinct chunk counted once.
        let mut unique: HashMap<Blake3Hash, u32> = HashMap::new();
        for s in &snaps {
            for f in &s.files {
                for c in &f.manifest.chunks {
                    unique.entry(c.hash).or_insert(c.len);
                }
            }
        }
        let unique_bytes = unique.values().map(|&l| u64::from(l)).sum();
        Ok(RepoStats {
            stored_bytes: self.store.disk_usage_bytes()?,
            unique_bytes,
            effective_bytes,
            snapshot_count: snaps.len() as u64,
            chunk_count: self.store.chunk_count()?,
        })
    }

    /// How many snapshots reference `hash` (0 if none). Derived on
    /// demand from the catalog — for diagnostics and tests.
    pub fn chunk_refcount(&self, hash: &Blake3Hash) -> Result<u64> {
        let mut count = 0u64;
        for snap in self.load_all_snapshots()? {
            if snap.distinct_chunks().contains(hash) {
                count += 1;
            }
        }
        Ok(count)
    }

    // ----- garbage collection ---------------------------------------

    /// Mark-and-sweep garbage collection.
    ///
    /// **Mark:** walk every manifest source — the snapshot catalog *and*
    /// the [`ChunkStore`]'s own Phase 27 `manifests` table (delta-resume
    /// baselines, and whatever the mount / recovery backends stored) —
    /// to build the reachable chunk set. Marking the Phase 27 manifests
    /// too is essential: the chunk store is shared, so a chunk reachable
    /// only through a delta-resume manifest must not be swept.
    ///
    /// **Sweep:** in a single pass over the chunk index, drop the index
    /// entry of any chunk not in the reachable set, then delete any
    /// non-active pack file whose chunks were *all* swept.
    ///
    /// Live data is never touched. GC takes the exclusive write lease,
    /// so it cannot run while a snapshot is mid-record (which would let
    /// it sweep chunks of a not-yet-committed snapshot).
    pub fn gc(&self) -> Result<GcReport> {
        let _lease = self
            .gc_lock
            .write()
            .map_err(|_| ChunkStoreError::Redb("repository gc lock poisoned".into()))?;

        // --- Mark: every chunk reachable from any manifest source.
        let mut reachable: HashSet<Blake3Hash> = HashSet::new();
        for snap in self.load_all_snapshots()? {
            for f in &snap.files {
                for c in &f.manifest.chunks {
                    reachable.insert(c.hash);
                }
            }
        }
        for manifest in self.store.all_manifests()? {
            for c in &manifest.chunks {
                reachable.insert(c.hash);
            }
        }

        // --- Sweep: one pass collects orphans + classifies packs.
        let all = self.store.all_locators()?;
        let active = self.store.active_pack_id()?;
        let mut orphans: Vec<Blake3Hash> = Vec::new();
        let mut live_packs: HashSet<Uuid> = HashSet::new();
        let mut all_packs: HashSet<Uuid> = HashSet::new();
        for (h, loc) in &all {
            all_packs.insert(loc.pack_id);
            if reachable.contains(h) {
                live_packs.insert(loc.pack_id);
            } else {
                orphans.push(*h);
            }
        }
        self.store.remove_chunk_index_entries(&orphans)?;

        let mut packs_removed = 0u64;
        let mut bytes_reclaimed = 0u64;
        for pid in all_packs {
            if pid != active && !live_packs.contains(&pid) {
                bytes_reclaimed += self.store.remove_pack_file(pid)?;
                packs_removed += 1;
            }
        }

        let report = GcReport {
            chunks_swept: orphans.len() as u64,
            packs_removed,
            bytes_reclaimed,
        };
        tracing::debug!(
            chunks_swept = report.chunks_swept,
            packs_removed = report.packs_removed,
            bytes_reclaimed = report.bytes_reclaimed,
            "repository gc complete"
        );
        Ok(report)
    }

    // ----- internals -------------------------------------------------

    /// Acquire the shared mutator lease (read side of the GC lock).
    fn read_lease(&self) -> Result<std::sync::RwLockReadGuard<'_, ()>> {
        self.gc_lock
            .read()
            .map_err(|_| ChunkStoreError::Redb("repository gc lock poisoned".into()))
    }

    /// Hand out the next monotonic snapshot id (starts at 1).
    fn allocate_snapshot_id(&self) -> Result<u64> {
        let txn = self.db.begin_write()?;
        let next;
        {
            let mut seq = txn.open_table(SEQ)?;
            let cur = seq
                .get(SEQ_NEXT_SNAPSHOT_ID)?
                .map(|v| v.value())
                .unwrap_or(1);
            next = cur;
            seq.insert(SEQ_NEXT_SNAPSHOT_ID, cur + 1)?;
        }
        txn.commit()?;
        Ok(next)
    }

    /// Load every snapshot (full, with manifests) from the catalog.
    fn load_all_snapshots(&self) -> Result<Vec<Snapshot>> {
        let txn = self.db.begin_read()?;
        let snaps = txn.open_table(SNAPSHOTS)?;
        let mut out = Vec::with_capacity(snaps.len()? as usize);
        for row in snaps.iter()? {
            let (_k, v) = row?;
            out.push(serde_json::from_slice::<Snapshot>(v.value())?);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lcg_bytes(seed: u64, len: usize) -> Vec<u8> {
        let mut out = vec![0u8; len];
        let mut s = seed;
        for b in &mut out {
            s = s
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            *b = (s >> 33) as u8;
        }
        out
    }

    #[test]
    fn open_lays_out_both_dbs() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = Repository::open(tmp.path()).unwrap();
        assert!(repo.root().join("index.redb").exists());
        assert!(repo.root().join("packs").exists());
        assert!(repo.root().join("repository.redb").exists());
    }

    #[test]
    fn record_then_list_and_fetch() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = Repository::open(tmp.path()).unwrap();
        let bytes = lcg_bytes(1, 1024 * 1024);
        let id = repo
            .snapshot_bytes(SnapshotKind::Copy, "first", 1000, &[("/a", &bytes)])
            .unwrap();
        assert_eq!(id.as_u64(), 1);

        let list = repo.snapshots().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].kind, SnapshotKind::Copy);
        assert_eq!(list[0].file_count, 1);
        assert_eq!(list[0].total_size, bytes.len() as u64);

        let full = repo.snapshot(id).unwrap().unwrap();
        assert_eq!(full.files.len(), 1);
        assert_eq!(full.files[0].path, "/a");
    }

    #[test]
    fn snapshots_ordered_by_time() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = Repository::open(tmp.path()).unwrap();
        let b = lcg_bytes(7, 4096);
        repo.snapshot_bytes(SnapshotKind::Sync, "later", 3000, &[("/x", &b)])
            .unwrap();
        repo.snapshot_bytes(SnapshotKind::Copy, "earlier", 1000, &[("/x", &b)])
            .unwrap();
        let list = repo.snapshots().unwrap();
        assert_eq!(list[0].created_at_ms, 1000);
        assert_eq!(list[1].created_at_ms, 3000);
    }

    #[test]
    fn snapshot_files_streams_and_restores() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = Repository::open(tmp.path()).unwrap();
        let bytes = lcg_bytes(99, 3 * 1024 * 1024);
        let src = tmp.path().join("src.bin");
        std::fs::write(&src, &bytes).unwrap();

        repo.snapshot_files(SnapshotKind::Backup, "stream", 1000, &[("/s", &src)])
            .unwrap();
        let fs = repo.snapshot_at("/s", 9999).unwrap().unwrap();
        let dst = tmp.path().join("out.bin");
        repo.restore(&fs, &dst).unwrap();
        assert_eq!(std::fs::read(&dst).unwrap(), bytes);
    }

    #[test]
    fn snapshot_at_picks_newest_at_or_before() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = Repository::open(tmp.path()).unwrap();
        let v1 = lcg_bytes(10, 4096);
        let mut v2 = v1.clone();
        v2[0] ^= 0xff;
        repo.snapshot_bytes(SnapshotKind::Version, "v1", 1000, &[("/f", &v1)])
            .unwrap();
        repo.snapshot_bytes(SnapshotKind::Version, "v2", 2000, &[("/f", &v2)])
            .unwrap();

        let at1500 = repo.snapshot_at("/f", 1500).unwrap().unwrap();
        assert_eq!(at1500.created_at_ms, 1000);
        let dst = tmp.path().join("out1");
        repo.restore(&at1500, &dst).unwrap();
        assert_eq!(std::fs::read(&dst).unwrap(), v1);

        let at2500 = repo.snapshot_at("/f", 2500).unwrap().unwrap();
        assert_eq!(at2500.created_at_ms, 2000);

        assert!(repo.snapshot_at("/f", 500).unwrap().is_none());
        assert!(repo.snapshot_at("/nope", 9999).unwrap().is_none());
    }

    #[test]
    fn stats_saved_ratio_reflects_dedup() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = Repository::open(tmp.path()).unwrap();
        let bytes = lcg_bytes(20, 2 * 1024 * 1024);
        // Same content in three snapshots → effective 3×, unique 1×.
        repo.snapshot_bytes(SnapshotKind::Copy, "s1", 1000, &[("/a", &bytes)])
            .unwrap();
        repo.snapshot_bytes(SnapshotKind::Sync, "s2", 2000, &[("/b", &bytes)])
            .unwrap();
        repo.snapshot_bytes(SnapshotKind::Version, "s3", 3000, &[("/c", &bytes)])
            .unwrap();
        let stats = repo.stats().unwrap();
        assert_eq!(stats.effective_bytes, 3 * bytes.len() as u64);
        // unique ≈ one copy of the content.
        assert!(stats.unique_bytes <= bytes.len() as u64 + 4096);
        // ~2/3 saved; never collapses to 0 despite pack overhead.
        assert!(stats.saved_ratio() > 0.6, "ratio {}", stats.saved_ratio());
    }

    #[test]
    fn chunk_refcount_counts_snapshots() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = Repository::open(tmp.path()).unwrap();
        let bytes = lcg_bytes(21, 2 * 1024 * 1024);
        let first = repo.chunker.chunk_bytes(&bytes)[0].hash;
        repo.snapshot_bytes(SnapshotKind::Copy, "s1", 1000, &[("/a", &bytes)])
            .unwrap();
        assert_eq!(repo.chunk_refcount(&first).unwrap(), 1);
        repo.snapshot_bytes(SnapshotKind::Sync, "s2", 2000, &[("/b", &bytes)])
            .unwrap();
        assert_eq!(repo.chunk_refcount(&first).unwrap(), 2);
    }

    #[test]
    fn gc_sweeps_orphans_keeps_live() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = Repository::open(tmp.path()).unwrap();
        let a = lcg_bytes(30, 2 * 1024 * 1024);
        let b = lcg_bytes(31, 2 * 1024 * 1024);
        let a_chunk = repo.chunker.chunk_bytes(&a)[0].hash;

        let s1 = repo
            .snapshot_bytes(SnapshotKind::Copy, "a", 1000, &[("/a", &a)])
            .unwrap();
        repo.snapshot_bytes(SnapshotKind::Version, "b", 2000, &[("/b", &b)])
            .unwrap();
        assert!(repo.store().has(&a_chunk).unwrap());

        let untouched = repo.gc().unwrap();
        assert_eq!(untouched.chunks_swept, 0, "gc must not touch live data");
        assert!(repo.store().has(&a_chunk).unwrap());

        assert!(repo.remove_snapshot(s1).unwrap());
        let report = repo.gc().unwrap();
        assert!(report.chunks_swept >= 1, "expected ≥1 sweep");
        assert!(!repo.store().has(&a_chunk).unwrap(), "orphan still present");
        assert_eq!(repo.chunk_refcount(&a_chunk).unwrap(), 0);

        let fs = repo.snapshot_at("/b", 9999).unwrap().unwrap();
        let dst = tmp.path().join("b.out");
        repo.restore(&fs, &dst).unwrap();
        assert_eq!(std::fs::read(&dst).unwrap(), b);
    }

    #[test]
    fn gc_preserves_phase27_manifest_chunks() {
        // A chunk referenced only by the ChunkStore's Phase 27 manifests
        // table (delta-resume / mount / recovery) — never by a Repository
        // snapshot — must survive gc.
        let tmp = tempfile::tempdir().unwrap();
        let repo = Repository::open(tmp.path()).unwrap();
        let data = lcg_bytes(42, 2 * 1024 * 1024);
        // Ingest via the Phase 27 path (writes the `manifests` table).
        let (_stats, manifest) =
            crate::ingest_bytes(repo.store(), &Chunker::default(), &data, "/delta/key").unwrap();
        let witness = manifest.chunks[0].hash;
        assert!(repo.store().has(&witness).unwrap());

        // No Repository snapshot references it, yet gc must keep it.
        let report = repo.gc().unwrap();
        assert_eq!(report.chunks_swept, 0, "Phase 27 chunks must be reachable");
        assert!(repo.store().has(&witness).unwrap());
    }

    #[test]
    fn gc_on_empty_repo_is_noop() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = Repository::open(tmp.path()).unwrap();
        let report = repo.gc().unwrap();
        assert_eq!(report.chunks_swept, 0);
        assert_eq!(report.packs_removed, 0);
    }
}
