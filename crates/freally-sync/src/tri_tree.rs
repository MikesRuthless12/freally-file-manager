//! Phase 52 — three-tree state machine for bidirectional sync.
//!
//! Phase 25 decides conflicts from a per-file vector clock plus a
//! single baseline record. This module models the same problem the way
//! Dropbox's Nucleus does: three complete trees, and every decision a
//! pure function of one path's三 states.
//!
//! - **LocalTree** — what is on this machine right now.
//! - **SyncedTree** — the last state both sides are known to have
//!   agreed on. This is the only durable state, and it is written
//!   through a two-phase commit.
//! - **RemoteTree** — what the peer has right now.
//!
//! # Why three trees beat a baseline plus clocks
//!
//! A vector clock tells you *that* two sides diverged; it does not tell
//! you what the user should see. With three trees, "local changed,
//! remote didn't" is a direct comparison rather than an inference, the
//! conflict UI can show all three states at once (which is what Phase
//! 52 asks for), and — most importantly — the synced tree can be
//! committed transactionally so a crash mid-sync cannot leave it
//! claiming an agreement that never happened.
//!
//! # The two-phase commit, and the bug it fixes
//!
//! A one-phase design writes the baseline right after the filesystem
//! action. Kill the process in that window and the baseline says
//! "agreed" for a copy that never landed — after which the file looks
//! *unchanged* on both sides forever and the difference is silently
//! never repaired. Here [`SyncedTree::prepare`] records the intent
//! first, the caller performs the filesystem work, and
//! [`SyncedTree::commit`] promotes it. A crash in between leaves a
//! prepared-but-uncommitted entry that [`SyncedTree::pending`] surfaces
//! on restart, and because `entries` was never touched the next
//! [`decide`] simply re-derives the correct action from fresh scans.
//!
//! # Status
//!
//! This is the state machine and its journal, with the full decision
//! matrix under test. It runs **alongside** Phase 25's engine rather
//! than replacing it: `freally-sync`'s shipped `sync()` entry point is
//! untouched, because swapping a working conflict engine in one step —
//! including migrating every existing `.freally-sync.db` — is a
//! separate, riskier change that deserves its own review.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

/// Content identity of one path on one side.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NodeState {
    /// No file at this path.
    Absent,
    /// Present, identified by content hash. Size rides along for the
    /// UI; identity is the hash alone, so a file rewritten with
    /// identical bytes is *not* a change.
    Present { hash: [u8; 32], size: u64 },
}

impl NodeState {
    pub fn present(hash: [u8; 32], size: u64) -> Self {
        Self::Present { hash, size }
    }

    pub fn is_present(&self) -> bool {
        matches!(self, Self::Present { .. })
    }

    fn hash(&self) -> Option<[u8; 32]> {
        match self {
            Self::Absent => None,
            Self::Present { hash, .. } => Some(*hash),
        }
    }
}

/// One path's three states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TriState {
    pub local: NodeState,
    pub synced: NodeState,
    pub remote: NodeState,
}

/// How a side differs from the synced tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SideChange {
    /// Identical to synced (including "absent on both").
    Unchanged,
    /// Present, and different from synced — an edit, or an add where
    /// synced was absent.
    Modified,
    /// Synced had content; this side no longer does.
    Deleted,
}

fn classify(side: NodeState, synced: NodeState) -> SideChange {
    match (side, synced) {
        (NodeState::Absent, NodeState::Absent) => SideChange::Unchanged,
        (NodeState::Absent, NodeState::Present { .. }) => SideChange::Deleted,
        (NodeState::Present { .. }, NodeState::Absent) => SideChange::Modified,
        (a, b) if a.hash() == b.hash() => SideChange::Unchanged,
        _ => SideChange::Modified,
    }
}

/// Why a path could not be resolved automatically.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TriConflict {
    /// Both sides changed the content differently.
    EditEdit,
    /// Local deleted, remote edited.
    DeleteEdit,
    /// Local edited, remote deleted.
    EditDelete,
    /// Both sides created the path with different content.
    AddAdd,
}

/// What the caller should do about one path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TriAction {
    /// Nothing to do; the trees already agree.
    Noop,
    /// Copy the remote's content over the local file.
    PullToLocal,
    /// Copy the local content over the remote file.
    PushToRemote,
    /// Delete the local file (the remote deleted it).
    DeleteLocal,
    /// Delete the remote file (the local side deleted it).
    DeleteRemote,
    /// Both sides reached the same content independently, or both
    /// deleted it. No I/O — just move the synced tree forward.
    AdvanceSynced,
    /// The path is gone everywhere; drop it from the synced tree so it
    /// stops being carried forever.
    DropFromSynced,
    /// Needs a human.
    Conflict(TriConflict),
}

/// Decide one path.
///
/// Total over all fifteen reachable combinations — see the test module,
/// which enumerates every one rather than sampling.
pub fn decide(state: &TriState) -> TriAction {
    let local = classify(state.local, state.synced);
    let remote = classify(state.remote, state.synced);

    // Converged independently: both sides hold identical content, so
    // whatever each did, there is nothing to transfer. Checked before
    // the change matrix because it subsumes several of its cells and is
    // never a conflict.
    if state.local.is_present()
        && state.remote.is_present()
        && state.local.hash() == state.remote.hash()
    {
        return if local == SideChange::Unchanged && remote == SideChange::Unchanged {
            TriAction::Noop
        } else {
            TriAction::AdvanceSynced
        };
    }

    match (local, remote) {
        (SideChange::Unchanged, SideChange::Unchanged) => {
            // Absent everywhere: only reachable when synced is also
            // absent, i.e. a stale entry nothing references.
            if state.synced.is_present() {
                TriAction::Noop
            } else {
                TriAction::DropFromSynced
            }
        }
        (SideChange::Unchanged, SideChange::Modified) => TriAction::PullToLocal,
        (SideChange::Modified, SideChange::Unchanged) => TriAction::PushToRemote,
        (SideChange::Unchanged, SideChange::Deleted) => TriAction::DeleteLocal,
        (SideChange::Deleted, SideChange::Unchanged) => TriAction::DeleteRemote,
        // Both gone: the deletion is agreed, so stop tracking it.
        (SideChange::Deleted, SideChange::Deleted) => TriAction::DropFromSynced,
        (SideChange::Deleted, SideChange::Modified) => TriAction::Conflict(TriConflict::DeleteEdit),
        (SideChange::Modified, SideChange::Deleted) => TriAction::Conflict(TriConflict::EditDelete),
        (SideChange::Modified, SideChange::Modified) => {
            // Distinguished for the UI: "you both created this" reads
            // very differently from "you both edited this".
            if state.synced.is_present() {
                TriAction::Conflict(TriConflict::EditEdit)
            } else {
                TriAction::Conflict(TriConflict::AddAdd)
            }
        }
    }
}

/// The synced state a successful `action` should leave behind.
///
/// `None` for the actions that need no synced entry afterwards, and for
/// conflicts — a conflict is explicitly *not* an agreement, so it must
/// never advance the synced tree.
pub fn intent_after(state: &TriState, action: TriAction) -> Option<NodeState> {
    match action {
        TriAction::Noop => None,
        TriAction::PullToLocal | TriAction::DeleteLocal => Some(state.remote),
        TriAction::PushToRemote | TriAction::DeleteRemote => Some(state.local),
        TriAction::AdvanceSynced => Some(state.local),
        TriAction::DropFromSynced => Some(NodeState::Absent),
        TriAction::Conflict(_) => None,
    }
}

/// A recorded-but-not-yet-committed synced-tree change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingOp {
    pub relpath: String,
    /// What the caller was told to do.
    pub action: TriAction,
    /// What `synced` becomes once that work succeeds.
    pub intent: NodeState,
}

/// The durable agreement, written through a two-phase commit.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncedTree {
    entries: BTreeMap<String, NodeState>,
    /// Prepared-but-uncommitted operations. Non-empty on load means the
    /// previous run died mid-sync.
    journal: BTreeMap<String, PendingOp>,
}

impl SyncedTree {
    pub fn new() -> Self {
        Self::default()
    }

    /// The agreed state of `relpath`. Unknown paths are `Absent` —
    /// never having seen a path and having agreed it is gone are the
    /// same thing to the decision matrix.
    pub fn get(&self, relpath: &str) -> NodeState {
        self.entries
            .get(relpath)
            .copied()
            .unwrap_or(NodeState::Absent)
    }

    /// Phase 1 — record the intent before the caller touches a file.
    ///
    /// A conflict is not recorded: it is not an agreement, and writing
    /// one would let a later commit silently bless it.
    pub fn prepare(&mut self, relpath: &str, action: TriAction, intent: NodeState) {
        if matches!(action, TriAction::Conflict(_)) {
            return;
        }
        self.journal.insert(
            relpath.to_string(),
            PendingOp {
                relpath: relpath.to_string(),
                action,
                intent,
            },
        );
    }

    /// Phase 2 — the filesystem work succeeded; make it durable.
    ///
    /// Returns `false` when there was nothing prepared for the path,
    /// which means the caller committed something it never prepared.
    pub fn commit(&mut self, relpath: &str) -> bool {
        let Some(op) = self.journal.remove(relpath) else {
            return false;
        };
        match op.intent {
            NodeState::Absent => {
                self.entries.remove(relpath);
            }
            present => {
                self.entries.insert(relpath.to_string(), present);
            }
        }
        true
    }

    /// The filesystem work failed — discard the intent, leaving the
    /// agreement exactly as it was.
    pub fn rollback(&mut self, relpath: &str) -> bool {
        self.journal.remove(relpath).is_some()
    }

    /// Operations prepared but never committed.
    ///
    /// Non-empty after a load means the last run was interrupted. The
    /// caller does **not** replay these blindly: it rescans both sides
    /// and re-runs [`decide`], which converges because `entries` was
    /// never advanced. They are surfaced so the caller can report the
    /// interruption and clear them.
    pub fn pending(&self) -> Vec<&PendingOp> {
        self.journal.values().collect()
    }

    /// Discard every prepared entry after a recovery rescan.
    pub fn clear_pending(&mut self) {
        self.journal.clear();
    }

    /// Every path the agreement knows about.
    pub fn paths(&self) -> impl Iterator<Item = &str> {
        self.entries.keys().map(String::as_str)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn to_json(&self) -> serde_json::Result<String> {
        serde_json::to_string(self)
    }

    pub fn from_json(s: &str) -> serde_json::Result<Self> {
        serde_json::from_str(s)
    }
}

/// One planned unit of work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedAction {
    pub relpath: String,
    pub state: TriState,
    pub action: TriAction,
}

/// Plan a whole sync pass.
///
/// The path set is the union of all three trees, so a file that exists
/// only in the agreement (deleted on both sides since) still gets a
/// decision instead of being carried forever.
pub fn plan(
    local: &BTreeMap<String, NodeState>,
    synced: &SyncedTree,
    remote: &BTreeMap<String, NodeState>,
) -> Vec<PlannedAction> {
    let mut paths: BTreeSet<&str> = BTreeSet::new();
    paths.extend(local.keys().map(String::as_str));
    paths.extend(remote.keys().map(String::as_str));
    paths.extend(synced.paths());

    paths
        .into_iter()
        .map(|relpath| {
            let state = TriState {
                local: local.get(relpath).copied().unwrap_or(NodeState::Absent),
                synced: synced.get(relpath),
                remote: remote.get(relpath).copied().unwrap_or(NodeState::Absent),
            };
            PlannedAction {
                relpath: relpath.to_string(),
                state,
                action: decide(&state),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(byte: u8) -> [u8; 32] {
        [byte; 32]
    }

    fn p(byte: u8) -> NodeState {
        NodeState::present(h(byte), 1)
    }

    const A: NodeState = NodeState::Absent;

    fn st(local: NodeState, synced: NodeState, remote: NodeState) -> TriState {
        TriState {
            local,
            synced,
            remote,
        }
    }

    // -----------------------------------------------------------------
    // The full matrix. Every reachable combination, not a sample.
    // -----------------------------------------------------------------

    #[test]
    fn synced_present_matrix() {
        // unchanged / unchanged
        assert_eq!(decide(&st(p(1), p(1), p(1))), TriAction::Noop);
        // unchanged / modified -> pull
        assert_eq!(decide(&st(p(1), p(1), p(2))), TriAction::PullToLocal);
        // modified / unchanged -> push
        assert_eq!(decide(&st(p(2), p(1), p(1))), TriAction::PushToRemote);
        // unchanged / deleted -> delete local
        assert_eq!(decide(&st(p(1), p(1), A)), TriAction::DeleteLocal);
        // deleted / unchanged -> delete remote
        assert_eq!(decide(&st(A, p(1), p(1))), TriAction::DeleteRemote);
        // deleted / deleted -> stop tracking
        assert_eq!(decide(&st(A, p(1), A)), TriAction::DropFromSynced);
        // deleted / modified -> conflict
        assert_eq!(
            decide(&st(A, p(1), p(2))),
            TriAction::Conflict(TriConflict::DeleteEdit)
        );
        // modified / deleted -> conflict
        assert_eq!(
            decide(&st(p(2), p(1), A)),
            TriAction::Conflict(TriConflict::EditDelete)
        );
        // modified / modified, different -> edit/edit
        assert_eq!(
            decide(&st(p(2), p(1), p(3))),
            TriAction::Conflict(TriConflict::EditEdit)
        );
        // modified / modified, SAME content -> converged, no I/O
        assert_eq!(decide(&st(p(2), p(1), p(2))), TriAction::AdvanceSynced);
    }

    #[test]
    fn synced_absent_matrix() {
        // Nothing anywhere: a stale agreement row to drop.
        assert_eq!(decide(&st(A, A, A)), TriAction::DropFromSynced);
        // New local only -> push
        assert_eq!(decide(&st(p(1), A, A)), TriAction::PushToRemote);
        // New remote only -> pull
        assert_eq!(decide(&st(A, A, p(1))), TriAction::PullToLocal);
        // Both added the same content -> converged
        assert_eq!(decide(&st(p(1), A, p(1))), TriAction::AdvanceSynced);
        // Both added different content -> add/add, distinct from edit/edit
        assert_eq!(
            decide(&st(p(1), A, p(2))),
            TriAction::Conflict(TriConflict::AddAdd)
        );
    }

    #[test]
    fn identity_is_content_not_size_or_mtime() {
        // Same hash, different recorded size: still unchanged. Identity
        // is the hash alone, so a rewrite with identical bytes is not a
        // change and must not cost a transfer.
        let local = NodeState::present(h(1), 10);
        let synced = NodeState::present(h(1), 999);
        assert_eq!(decide(&st(local, synced, synced)), TriAction::Noop);
    }

    // -----------------------------------------------------------------
    // Two-phase commit
    // -----------------------------------------------------------------

    #[test]
    fn commit_advances_the_agreement() {
        let mut tree = SyncedTree::new();
        let state = st(p(2), p(1), p(1));
        let action = decide(&state);
        assert_eq!(action, TriAction::PushToRemote);

        tree.prepare("a.txt", action, intent_after(&state, action).unwrap());
        // Still not agreed — the file work has not happened yet.
        assert_eq!(tree.get("a.txt"), A);
        assert_eq!(tree.pending().len(), 1);

        assert!(tree.commit("a.txt"));
        assert_eq!(tree.get("a.txt"), p(2));
        assert!(tree.pending().is_empty());
    }

    #[test]
    fn a_crash_between_the_phases_leaves_the_agreement_untouched() {
        // This is the bug the two-phase commit exists for: a one-phase
        // design would have recorded agreement for a copy that never
        // landed, after which both sides look "unchanged" forever and
        // the difference is never repaired.
        let mut tree = SyncedTree::new();
        tree.entries.insert("a.txt".into(), p(1));

        let state = st(p(2), p(1), p(1));
        let action = decide(&state);
        tree.prepare("a.txt", action, intent_after(&state, action).unwrap());

        // Process dies here — persist and reload.
        let reloaded = SyncedTree::from_json(&tree.to_json().unwrap()).unwrap();

        assert_eq!(
            reloaded.get("a.txt"),
            p(1),
            "the agreement must not have advanced"
        );
        assert_eq!(reloaded.pending().len(), 1, "the interruption is visible");

        // Recovery: rescan, re-decide, and the same work is planned.
        let rescanned = st(p(2), reloaded.get("a.txt"), p(1));
        assert_eq!(decide(&rescanned), TriAction::PushToRemote);
    }

    #[test]
    fn rollback_leaves_the_agreement_exactly_as_it_was() {
        let mut tree = SyncedTree::new();
        tree.entries.insert("a.txt".into(), p(1));
        let state = st(p(2), p(1), p(1));
        let action = decide(&state);
        tree.prepare("a.txt", action, intent_after(&state, action).unwrap());

        assert!(tree.rollback("a.txt"));
        assert_eq!(tree.get("a.txt"), p(1));
        assert!(tree.pending().is_empty());
    }

    #[test]
    fn a_conflict_never_enters_the_journal() {
        // Recording a conflict would let a later commit bless a state
        // no one agreed to.
        let mut tree = SyncedTree::new();
        let state = st(p(2), p(1), p(3));
        let action = decide(&state);
        assert!(matches!(action, TriAction::Conflict(_)));
        assert_eq!(intent_after(&state, action), None);

        tree.prepare("a.txt", action, NodeState::Absent);
        assert!(tree.pending().is_empty());
        assert!(!tree.commit("a.txt"), "nothing was prepared to commit");
    }

    #[test]
    fn deleting_removes_the_row_rather_than_storing_absent() {
        let mut tree = SyncedTree::new();
        tree.entries.insert("gone.txt".into(), p(1));
        let state = st(A, p(1), A);
        let action = decide(&state);
        assert_eq!(action, TriAction::DropFromSynced);
        tree.prepare("gone.txt", action, intent_after(&state, action).unwrap());
        assert!(tree.commit("gone.txt"));
        assert_eq!(tree.get("gone.txt"), A);
        assert!(tree.is_empty(), "an agreed deletion stops being carried");
    }

    #[test]
    fn committing_something_never_prepared_is_refused() {
        let mut tree = SyncedTree::new();
        assert!(!tree.commit("nope.txt"));
        assert_eq!(tree.get("nope.txt"), A);
    }

    // -----------------------------------------------------------------
    // Planning
    // -----------------------------------------------------------------

    #[test]
    fn plan_covers_the_union_including_agreement_only_paths() {
        let mut local = BTreeMap::new();
        local.insert("both.txt".to_string(), p(1));
        local.insert("local-only.txt".to_string(), p(5));

        let mut remote = BTreeMap::new();
        remote.insert("both.txt".to_string(), p(1));
        remote.insert("remote-only.txt".to_string(), p(6));

        let mut synced = SyncedTree::new();
        synced.entries.insert("both.txt".into(), p(1));
        // Present only in the agreement: deleted on both sides.
        synced.entries.insert("vanished.txt".into(), p(9));

        let planned = plan(&local, &synced, &remote);
        let by_path: BTreeMap<&str, TriAction> = planned
            .iter()
            .map(|a| (a.relpath.as_str(), a.action))
            .collect();

        assert_eq!(by_path["both.txt"], TriAction::Noop);
        assert_eq!(by_path["local-only.txt"], TriAction::PushToRemote);
        assert_eq!(by_path["remote-only.txt"], TriAction::PullToLocal);
        assert_eq!(
            by_path["vanished.txt"],
            TriAction::DropFromSynced,
            "a path only the agreement remembers must still be decided"
        );
        assert_eq!(planned.len(), 4);
    }

    #[test]
    fn a_full_pass_converges_and_is_idempotent() {
        let mut local = BTreeMap::new();
        local.insert("a.txt".to_string(), p(2));
        let remote = BTreeMap::new();
        let mut synced = SyncedTree::new();

        // First pass: push the new local file.
        let planned = plan(&local, &synced, &remote);
        assert_eq!(planned[0].action, TriAction::PushToRemote);
        let intent = intent_after(&planned[0].state, planned[0].action).unwrap();
        synced.prepare("a.txt", planned[0].action, intent);
        synced.commit("a.txt");

        // The remote now has it; a second pass must do nothing.
        let mut remote = BTreeMap::new();
        remote.insert("a.txt".to_string(), p(2));
        let planned = plan(&local, &synced, &remote);
        assert_eq!(planned[0].action, TriAction::Noop);
    }
}
