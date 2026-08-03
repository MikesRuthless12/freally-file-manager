//! FFM-M20 — favorites & recent pairs.
//!
//! Bookmarks for sources, destinations, and source→destination
//! **pairs**, plus an automatically-maintained ring of recently-used
//! pairs. Phase 45's tray-pinned destinations were tray-menu-only and
//! destination-only; these are first-class, labelled, hotkeyable, and
//! surfaced in the drop dialog and pickers.
//!
//! ## Portable-mode paths
//!
//! A bookmark on a portable install is stored **relative to the
//! portable root** whenever it points inside it, so the same stick
//! works when it mounts as `E:` on one machine and `/media/usb` on the
//! next. Bookmarks pointing at the host's own disk stay absolute —
//! they were never portable. See `freally_settings::portable`.

use freally_settings::{FavoriteEntry, RecentPair, portable};
use serde::{Deserialize, Serialize};

use crate::ipc_safety::{err_string, validate_ipc_path};
use crate::state::AppState;

/// Defensive caps at the IPC boundary — the renderer is only
/// part-trusted, and both lists are rendered into menus.
const MAX_FAVORITES: usize = 200;
const MAX_RECENT_PAIRS: usize = 20;
const MAX_LABEL_CHARS: usize = 64;

/// Wire shape for one bookmark.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FavoriteDto {
    /// Stable id. A blank id on save means "create"; the backend mints
    /// one so two rapid creates can't collide on a renderer-side guess.
    #[serde(default)]
    pub id: String,
    /// User-facing label.
    pub label: String,
    /// `"source"` | `"destination"` | `"pair"`.
    pub kind: String,
    /// The bookmarked path (the source side of a pair). Always
    /// absolute on the wire, even when stored relative.
    pub path: String,
    /// Destination side, for `kind == "pair"`.
    #[serde(default)]
    pub destination: String,
    /// Optional accelerator. Empty = none.
    #[serde(default)]
    pub hotkey: String,
}

/// Wire shape for one remembered pair.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecentPairDto {
    /// Source root.
    pub source: String,
    /// Destination root.
    pub destination: String,
    /// Last use, ms since epoch.
    pub used_at_ms: i64,
}

impl From<&RecentPair> for RecentPairDto {
    fn from(p: &RecentPair) -> Self {
        Self {
            source: portable::absolutize(std::path::Path::new(&p.source))
                .to_string_lossy()
                .into_owned(),
            destination: portable::absolutize(std::path::Path::new(&p.destination))
                .to_string_lossy()
                .into_owned(),
            used_at_ms: p.used_at_ms,
        }
    }
}

impl From<&FavoriteEntry> for FavoriteDto {
    fn from(e: &FavoriteEntry) -> Self {
        Self {
            id: e.id.clone(),
            label: e.label.clone(),
            kind: e.kind.clone(),
            // Stored relative under portable mode; the UI always sees
            // an absolute path so pickers and the engine agree.
            path: portable::absolutize(std::path::Path::new(&e.path))
                .to_string_lossy()
                .into_owned(),
            destination: if e.destination.is_empty() {
                String::new()
            } else {
                portable::absolutize(std::path::Path::new(&e.destination))
                    .to_string_lossy()
                    .into_owned()
            },
            hotkey: e.hotkey.clone(),
        }
    }
}

/// Mint an id for a new bookmark.
///
/// Derived from the label plus a monotonic suffix rather than a random
/// uuid, so it stays readable in `settings.toml` and this crate does
/// not grow a uuid dependency for one field.
fn mint_id(label: &str, existing: &[FavoriteEntry]) -> String {
    let stem: String = label
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .chars()
        .take(24)
        .collect();
    let stem = if stem.is_empty() {
        "favorite".to_string()
    } else {
        stem
    };
    if !existing.iter().any(|e| e.id == stem) {
        return stem;
    }
    // Bounded by the list length: with `n` entries, one of
    // `stem-2 ..= stem-(n+2)` is always free, so there is no
    // unreachable tail that could hand back a colliding id.
    for n in 2..=(existing.len() as u32 + 2) {
        let candidate = format!("{stem}-{n}");
        if !existing.iter().any(|e| e.id == candidate) {
            return candidate;
        }
    }
    unreachable!("a free suffix exists within existing.len() + 1 candidates")
}

/// Validate a DTO and normalise it into a persistable entry.
pub fn entry_from_dto(
    dto: &FavoriteDto,
    existing: &[FavoriteEntry],
) -> Result<FavoriteEntry, String> {
    let label = dto.label.trim();
    if label.is_empty() || label.chars().count() > MAX_LABEL_CHARS {
        return Err("err-favorite-label-invalid".to_string());
    }
    if label.chars().any(char::is_control) || label.contains('\u{FFFD}') {
        return Err("err-favorite-label-invalid".to_string());
    }
    if !matches!(dto.kind.as_str(), "source" | "destination" | "pair") {
        return Err("err-favorite-kind-invalid".to_string());
    }

    let path = validate_ipc_path(&dto.path).map_err(err_string)?;
    let destination = if dto.kind == "pair" {
        let d = validate_ipc_path(&dto.destination).map_err(err_string)?;
        portable::relativize(&d).to_string_lossy().into_owned()
    } else {
        // A non-pair bookmark has no destination side; drop whatever
        // the caller sent rather than persisting a field that the kind
        // says is meaningless.
        String::new()
    };

    if dto.hotkey.chars().any(char::is_control) || dto.hotkey.chars().count() > 64 {
        return Err("err-favorite-hotkey-invalid".to_string());
    }

    let id = if dto.id.trim().is_empty() {
        mint_id(label, existing)
    } else {
        // An explicit id is an edit of an existing row, but it still
        // arrives from a part-trusted renderer and lands in
        // `settings.toml`. Bound it rather than persisting an arbitrary
        // string 200 times over.
        let id = dto.id.trim();
        if id.chars().count() > 64 || id.chars().any(char::is_control) {
            return Err("err-favorite-id-invalid".to_string());
        }
        id.to_string()
    };

    Ok(FavoriteEntry {
        id,
        label: label.to_string(),
        kind: dto.kind.clone(),
        path: portable::relativize(&path).to_string_lossy().into_owned(),
        destination,
        hotkey: dto.hotkey.trim().to_string(),
    })
}

/// Insert `pair` at the head of `ring`, de-duplicating on the
/// (source, destination) key and capping the length.
///
/// Pure so the LRU semantics are testable without a settings file.
pub fn push_recent(ring: &mut Vec<RecentPair>, pair: RecentPair) {
    ring.retain(|p| !(p.source == pair.source && p.destination == pair.destination));
    ring.insert(0, pair);
    ring.truncate(MAX_RECENT_PAIRS);
}

// ---------------------------------------------------------------------
// Tauri commands
// ---------------------------------------------------------------------

/// `favorites_list()` — bookmarks in user order.
#[tauri::command]
pub fn favorites_list(state: tauri::State<'_, AppState>) -> Vec<FavoriteDto> {
    favorites_list_impl(state.inner())
}

/// `favorites_save(dto)` — create (blank id) or update a bookmark.
#[tauri::command]
pub fn favorites_save(
    state: tauri::State<'_, AppState>,
    dto: FavoriteDto,
) -> Result<Vec<FavoriteDto>, String> {
    favorites_save_impl(state.inner(), &dto)
}

/// `favorites_remove(id)` — drop a bookmark. Idempotent.
#[tauri::command]
pub fn favorites_remove(
    state: tauri::State<'_, AppState>,
    id: String,
) -> Result<Vec<FavoriteDto>, String> {
    favorites_remove_impl(state.inner(), &id)
}

/// `favorites_recent_pairs()` — the MRU pair ring, newest first.
#[tauri::command]
pub fn favorites_recent_pairs(state: tauri::State<'_, AppState>) -> Vec<RecentPairDto> {
    state
        .settings_snapshot()
        .favorites
        .recent_pairs
        .iter()
        .map(RecentPairDto::from)
        .collect()
}

/// `favorites_record_pair(source, destination)` — remember a
/// source→destination pair the user just ran.
#[tauri::command]
pub fn favorites_record_pair(
    state: tauri::State<'_, AppState>,
    source: String,
    destination: String,
    used_at_ms: i64,
) -> Result<Vec<RecentPairDto>, String> {
    favorites_record_pair_impl(state.inner(), &source, &destination, used_at_ms)
}

// ---------------------------------------------------------------------
// Implementations — public for tests
// ---------------------------------------------------------------------

/// Implementation of [`favorites_list`]. Public for tests.
pub fn favorites_list_impl(state: &AppState) -> Vec<FavoriteDto> {
    state
        .settings_snapshot()
        .favorites
        .entries
        .iter()
        .map(FavoriteDto::from)
        .collect()
}

/// Implementation of [`favorites_save`]. Public for tests.
pub fn favorites_save_impl(
    state: &AppState,
    dto: &FavoriteDto,
) -> Result<Vec<FavoriteDto>, String> {
    let mut s = state.settings.write().unwrap_or_else(|p| p.into_inner());
    let entry = entry_from_dto(dto, &s.favorites.entries)?;
    match s.favorites.entries.iter_mut().find(|e| e.id == entry.id) {
        Some(existing) => *existing = entry,
        None => {
            if s.favorites.entries.len() >= MAX_FAVORITES {
                return Err("err-favorite-too-many".to_string());
            }
            s.favorites.entries.push(entry);
        }
    }
    state.persist_settings(&s)?;
    Ok(s.favorites.entries.iter().map(FavoriteDto::from).collect())
}

/// Implementation of [`favorites_remove`]. Public for tests.
pub fn favorites_remove_impl(state: &AppState, id: &str) -> Result<Vec<FavoriteDto>, String> {
    let mut s = state.settings.write().unwrap_or_else(|p| p.into_inner());
    s.favorites.entries.retain(|e| e.id != id);
    state.persist_settings(&s)?;
    Ok(s.favorites.entries.iter().map(FavoriteDto::from).collect())
}

/// Implementation of [`favorites_record_pair`]. Public for tests.
pub fn favorites_record_pair_impl(
    state: &AppState,
    source: &str,
    destination: &str,
    used_at_ms: i64,
) -> Result<Vec<RecentPairDto>, String> {
    let src = validate_ipc_path(source).map_err(err_string)?;
    let dst = validate_ipc_path(destination).map_err(err_string)?;
    let mut s = state.settings.write().unwrap_or_else(|p| p.into_inner());
    push_recent(
        &mut s.favorites.recent_pairs,
        RecentPair {
            source: portable::relativize(&src).to_string_lossy().into_owned(),
            destination: portable::relativize(&dst).to_string_lossy().into_owned(),
            used_at_ms,
        },
    );
    state.persist_settings(&s)?;
    Ok(s.favorites
        .recent_pairs
        .iter()
        .map(RecentPairDto::from)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn abs(p: &str) -> String {
        if cfg!(windows) {
            format!(r"C:\{p}")
        } else {
            format!("/{p}")
        }
    }

    fn dto(kind: &str) -> FavoriteDto {
        FavoriteDto {
            id: String::new(),
            label: "Photos 2026".to_string(),
            kind: kind.to_string(),
            path: abs("photos"),
            destination: abs("backup"),
            hotkey: String::new(),
        }
    }

    #[test]
    fn a_new_bookmark_gets_a_readable_minted_id() {
        let entry = entry_from_dto(&dto("source"), &[]).expect("valid");
        assert_eq!(entry.id, "photos-2026");
    }

    #[test]
    fn minted_ids_never_collide() {
        let mut existing = vec![entry_from_dto(&dto("source"), &[]).unwrap()];
        let second = entry_from_dto(&dto("source"), &existing).unwrap();
        assert_eq!(second.id, "photos-2026-2");
        existing.push(second);
        let third = entry_from_dto(&dto("source"), &existing).unwrap();
        assert_eq!(third.id, "photos-2026-3");
    }

    #[test]
    fn a_label_of_only_punctuation_still_yields_a_usable_id() {
        let mut d = dto("source");
        d.label = "!!!".to_string();
        assert_eq!(entry_from_dto(&d, &[]).unwrap().id, "favorite");
    }

    #[test]
    fn a_non_pair_bookmark_drops_the_destination_field() {
        for kind in ["source", "destination"] {
            let entry = entry_from_dto(&dto(kind), &[]).unwrap();
            assert!(
                entry.destination.is_empty(),
                "{kind} must not persist a destination it cannot use",
            );
        }
        let pair = entry_from_dto(&dto("pair"), &[]).unwrap();
        assert!(!pair.destination.is_empty());
    }

    #[test]
    fn an_invalid_kind_is_refused() {
        let mut d = dto("source");
        d.kind = "folder".to_string();
        assert_eq!(
            entry_from_dto(&d, &[]).unwrap_err(),
            "err-favorite-kind-invalid"
        );
    }

    #[test]
    fn a_label_with_control_characters_or_replacement_chars_is_refused() {
        for bad in ["a\nb", "a\u{FFFD}b", "", "   "] {
            let mut d = dto("source");
            d.label = bad.to_string();
            assert_eq!(
                entry_from_dto(&d, &[]).unwrap_err(),
                "err-favorite-label-invalid",
                "{bad:?}"
            );
        }
    }

    #[test]
    fn a_traversal_path_is_refused_by_the_ipc_gate() {
        let mut d = dto("source");
        d.path = abs("photos/../../secrets");
        assert!(entry_from_dto(&d, &[]).is_err());
    }

    #[test]
    fn a_pair_with_a_bad_destination_is_refused_even_when_the_source_is_fine() {
        let mut d = dto("pair");
        d.destination = abs("backup/../../etc");
        assert!(entry_from_dto(&d, &[]).is_err());
    }

    #[test]
    fn saving_with_an_explicit_id_updates_rather_than_duplicates() {
        let first = entry_from_dto(&dto("source"), &[]).unwrap();
        let mut d = dto("source");
        d.id = first.id.clone();
        d.label = "Photos (renamed)".to_string();
        let updated = entry_from_dto(&d, std::slice::from_ref(&first)).unwrap();
        assert_eq!(updated.id, first.id);
        assert_eq!(updated.label, "Photos (renamed)");
    }

    #[test]
    fn the_recent_ring_is_mru_ordered_and_deduplicated() {
        let mut ring = Vec::new();
        for i in 0..3 {
            push_recent(
                &mut ring,
                RecentPair {
                    source: format!("/s{i}"),
                    destination: "/d".to_string(),
                    used_at_ms: i,
                },
            );
        }
        assert_eq!(ring[0].source, "/s2", "newest first");

        // Re-running an older pair moves it to the head instead of
        // appending a second row.
        push_recent(
            &mut ring,
            RecentPair {
                source: "/s0".to_string(),
                destination: "/d".to_string(),
                used_at_ms: 99,
            },
        );
        assert_eq!(ring.len(), 3, "a repeat must not grow the ring");
        assert_eq!(ring[0].source, "/s0");
        assert_eq!(ring[0].used_at_ms, 99, "the timestamp must refresh");
    }

    #[test]
    fn the_recent_ring_is_capped() {
        let mut ring = Vec::new();
        for i in 0..(MAX_RECENT_PAIRS as i64 + 5) {
            push_recent(
                &mut ring,
                RecentPair {
                    source: format!("/s{i}"),
                    destination: "/d".to_string(),
                    used_at_ms: i,
                },
            );
        }
        assert_eq!(ring.len(), MAX_RECENT_PAIRS);
        assert_eq!(
            ring[0].source,
            format!("/s{}", MAX_RECENT_PAIRS + 4),
            "the cap must drop the oldest, not the newest",
        );
    }

    #[test]
    fn the_same_source_to_a_different_destination_is_a_distinct_pair() {
        let mut ring = Vec::new();
        push_recent(
            &mut ring,
            RecentPair {
                source: "/s".to_string(),
                destination: "/d1".to_string(),
                used_at_ms: 1,
            },
        );
        push_recent(
            &mut ring,
            RecentPair {
                source: "/s".to_string(),
                destination: "/d2".to_string(),
                used_at_ms: 2,
            },
        );
        assert_eq!(ring.len(), 2);
    }
}
