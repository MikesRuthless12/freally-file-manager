//! `freally-sync` — two-way folder sync with vector-clock conflict
//! detection.
//!
//! A `SyncPair` pins two folders (left, right) plus a persistent
//! state DB at `<pair-root>/.freally-sync.db`. [`sync`] reconciles
//! the two sides against the last-seen baseline stored in the DB and
//! produces a [`SyncReport`] with counts of applied actions plus a
//! list of unresolved conflicts. Conflicts write preservation files
//! named `name.sync-conflict-YYYYMMDD-HHMMSS-<host>.ext` so no data
//! is lost even when both sides edited the same relpath concurrently.
//!
//! # Storage
//!
//! Single redb file per pair. Three logical tables:
//!
//! - `device`: a `&str → &[u8]` one-row store for the local device's
//!   persisted UUID. Generated on first open, kept for life — that's
//!   how the vector-clock algorithm tells "this host" from "that host"
//!   across sync runs.
//! - `files`: `&str (relpath) → &str (JSON FileRecord)` — the
//!   last-seen-synced state for each file, including the merged
//!   vector clock, mtime, size, and BLAKE3 digest. When a file hasn't
//!   been synced yet on either side, the entry is absent.
//! - `history`: `(u64 ts_ms, u64 seq) → &str (JSON HistoryEntry)` — a
//!   rolling audit log of applied actions; the UI renders it in the
//!   pair's detail view.
//!
//! All writes go through redb `WriteTransaction`s (ACID + fsync on
//! commit), so a torn write at the OS level can never leave the sync
//! DB in a half-committed state.
//!
//! # Sync algorithm
//!
//! One `sync()` call is one reconciliation round:
//!
//! 1. **Walk both sides.** Record current [`SideState`] (mtime, size,
//!    BLAKE3) for every relpath seen on either side.
//! 2. **Decide per file.** For each relpath, consult the baseline in
//!    `files`, compare against the two current states, and produce a
//!    [`SyncAction`] per the matrix in [`engine::decide_action`].
//! 3. **Execute.** Copies run first, then deletes — the order matters
//!    for rename detection (modelled as delete+add by the spec). Each
//!    completed action updates the baseline row and appends one entry
//!    to `history`.
//! 4. **Surface conflicts.** Concurrent writes and delete-edit cases
//!    produce a [`SyncAction::KeepConflict`]; the engine preserves the
//!    losing side's content with a dated-suffix filename on the losing
//!    side, copies the winner's content to the loser's canonical path,
//!    and records the pair in the returned [`SyncReport`] so the UI
//!    can surface a per-file resolution dialog.
//!
//! Pause / resume / cancel mirror the freally-core `CopyControl`
//! shape — see [`SyncControl`].
//!
//! # Vector clocks: recorded, not yet consulted
//!
//! [`VersionVector`] is maintained per file and persisted alongside
//! the baseline, and [`clock::resolve_concurrent`] merges the two
//! sides after a winner is chosen. That is the whole of it today.
//!
//! The engine decides which side wins from presence and mtime
//! against the baseline — it never calls [`VersionVector::compare`],
//! so the ancestor / concurrent / equal distinction the vectors could
//! draw does not currently influence any outcome. This section used
//! to describe that distinction as the algorithm in use; it is the
//! algorithm the recorded data would *support*, once something reads
//! it. The vectors are written now so the history is already there
//! when it is.

#![forbid(unsafe_code)]

pub mod clock;
pub mod control;
pub mod db;
pub mod engine;
pub mod error;
pub mod tri_tree;
pub mod types;
pub mod walker;

pub use clock::{DeviceId, VersionVector, VvCompare};
pub use control::{SyncControl, SyncState};
pub use db::{FileRecord, HistoryEntry, HistoryKind, SyncDb, default_pair_db_path};
pub use engine::sync;
pub use error::{Result, SyncError};
pub use tri_tree::{
    NodeState, PendingOp, PlannedAction, SyncedTree, TriAction, TriConflict, TriState, decide,
    intent_after, plan,
};
pub use types::{
    Conflict, ConflictKind, Direction, FileMeta, SideState, SyncAction, SyncEvent, SyncMode,
    SyncOptions, SyncPair, SyncReport,
};
pub use walker::scan_side;
