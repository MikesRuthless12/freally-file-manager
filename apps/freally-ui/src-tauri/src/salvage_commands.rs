//! FFM-M15 — damaged-media salvage mode v1.
//!
//! A normal verified copy is the wrong tool for a dying disk: one
//! unreadable sector fails the file, and the next attempt re-reads the
//! good gigabytes it already had. Salvage inverts that — it recovers
//! everything readable, maps what it could not read, and never re-reads
//! what it already has.
//!
//! The ladder, per file:
//!
//! 1. Read in blocks (1 MiB by default).
//! 2. A failing block is retried up to a per-block budget, then
//!    **halved** and each half retried — down to a 4 KiB floor. This is
//!    what "shrink read sizes on error" buys: a single bad sector costs
//!    one 4 KiB hole, not a 1 MiB one.
//! 3. A range that still fails at the floor is **skipped and mapped**:
//!    the destination is zero-filled across it so every later byte
//!    keeps its correct offset, and the range is recorded as a gap.
//! 4. The gaps are written to a `.freally-salvage.json` manifest beside
//!    the destination. Re-running reads that manifest and re-attempts
//!    **only the gaps** — a second pass on a drive that has cooled down
//!    (or a second copy of the same disc) costs minutes, not hours.
//!
//! Salvaged output is intentionally **not** byte-exact, so it is
//! flagged distinctly: a run that ends with gaps records its history
//! item as `salvaged` rather than `ok`, which is what the FFM-M10
//! certificate and FFM-M07 ledger then show.
//!
//! Salvage never engages on its own — a healthy copy never takes this
//! path. It is an explicit action on an explicit file.
//!
//! The ladder itself is written against a [`BlockReader`] trait so the
//! whole failure matrix is unit-tested against a fault-injecting fake;
//! testing it on real dying hardware is not something a test suite can
//! do.

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tauri::State;

use freally_history::{ItemRow, JobSummary};

use crate::ipc_safety::validate_ipc_path;
use crate::report_util::{now_ms, with_suffix};
use crate::state::AppState;

/// Default first rung of the ladder.
const DEFAULT_BLOCK: usize = 1024 * 1024;
/// Smallest read the ladder will descend to — one page. Below this the
/// syscall overhead dominates and the recovered granularity stops
/// improving on real media.
const MIN_BLOCK: usize = 4096;
/// How many times one read is retried before the range is halved.
const DEFAULT_RETRIES: u32 = 2;
/// Manifest filename suffix, appended to the destination file name.
const MANIFEST_SUFFIX: &str = ".freally-salvage.json";

/// A range of the source that could not be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Gap {
    pub offset: u64,
    pub len: u64,
}

/// The on-disk gap manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SalvageManifest {
    pub source: String,
    pub destination: String,
    /// Source size the gaps refer to. A changed size invalidates them.
    pub size: u64,
    pub gaps: Vec<Gap>,
    pub recovered_bytes: u64,
    pub updated_at_ms: i64,
}

/// Random-access reader the ladder drives. Real runs use a `File`;
/// tests use a fake that fails chosen ranges.
pub trait BlockReader {
    /// Read exactly `buf.len()` bytes at `offset`, or fail.
    fn read_exact_at(&mut self, offset: u64, buf: &mut [u8]) -> std::io::Result<()>;
}

impl BlockReader for std::fs::File {
    fn read_exact_at(&mut self, offset: u64, buf: &mut [u8]) -> std::io::Result<()> {
        self.seek(SeekFrom::Start(offset))?;
        self.read_exact(buf)
    }
}

/// Where recovered bytes go. Separated from the reader so the ladder is
/// testable without touching a filesystem.
pub trait BlockWriter {
    fn write_all_at(&mut self, offset: u64, buf: &[u8]) -> std::io::Result<()>;
}

impl BlockWriter for std::fs::File {
    fn write_all_at(&mut self, offset: u64, buf: &[u8]) -> std::io::Result<()> {
        self.seek(SeekFrom::Start(offset))?;
        self.write_all(buf)
    }
}

/// Tunables. Defaults match the constants above; the fields exist so
/// the tests can drive a small ladder without megabyte buffers.
#[derive(Debug, Clone, Copy)]
pub struct SalvageOptions {
    pub block: usize,
    pub min_block: usize,
    pub retries: u32,
}

impl Default for SalvageOptions {
    fn default() -> Self {
        Self {
            block: DEFAULT_BLOCK,
            min_block: MIN_BLOCK,
            retries: DEFAULT_RETRIES,
        }
    }
}

/// Recover the range `buf` covers, starting at `offset`, halving on
/// failure and zero-filling whatever stays unreadable.
///
/// `buf` is the caller's scratch space, sliced down through the
/// halving rather than reallocated: salvaging a 20 GB image would
/// otherwise allocate *and zero* a fresh 1 MiB buffer per block, and
/// the read overwrites every byte of it anyway.
///
/// Returns the gaps it could not recover, in ascending offset order,
/// and adds the recovered byte count to `recovered`.
fn salvage_range(
    reader: &mut dyn BlockReader,
    writer: &mut dyn BlockWriter,
    offset: u64,
    buf: &mut [u8],
    opts: &SalvageOptions,
    recovered: &mut u64,
) -> Vec<Gap> {
    let len = buf.len();
    let mut attempts = 0;
    loop {
        match reader.read_exact_at(offset, buf) {
            Ok(()) => {
                if writer.write_all_at(offset, buf).is_err() {
                    // A destination that cannot be written is not a
                    // source defect; report the range as a gap so the
                    // manifest still describes what is missing.
                    return vec![Gap {
                        offset,
                        len: len as u64,
                    }];
                }
                *recovered += len as u64;
                return Vec::new();
            }
            Err(_) if attempts < opts.retries => attempts += 1,
            Err(_) => break,
        }
    }

    // Out of retries. Halve, unless we are already at the floor — then
    // this range is genuinely unreadable: zero-fill it so every later
    // byte keeps its true offset, and map it.
    if len <= opts.min_block {
        buf.fill(0);
        let _ = writer.write_all_at(offset, buf);
        return vec![Gap {
            offset,
            len: len as u64,
        }];
    }

    let half = len / 2;
    let (head, tail) = buf.split_at_mut(half);
    let mut gaps = salvage_range(reader, writer, offset, head, opts, recovered);
    gaps.extend(salvage_range(
        reader,
        writer,
        offset + half as u64,
        tail,
        opts,
        recovered,
    ));
    // The two halves come back in ascending order and every caller
    // merges the concatenated result, so no merge is needed here.
    gaps
}

/// Collapse touching gaps so the manifest stays readable — a bad
/// 1 MiB region should be one entry, not 256.
fn merge_adjacent(gaps: Vec<Gap>) -> Vec<Gap> {
    let mut out: Vec<Gap> = Vec::with_capacity(gaps.len());
    for gap in gaps {
        match out.last_mut() {
            Some(last) if last.offset + last.len == gap.offset => last.len += gap.len,
            _ => out.push(gap),
        }
    }
    out
}

/// Walk the whole file through the ladder.
pub fn salvage_whole(
    reader: &mut dyn BlockReader,
    writer: &mut dyn BlockWriter,
    size: u64,
    opts: &SalvageOptions,
) -> (Vec<Gap>, u64) {
    let mut recovered = 0u64;
    let mut gaps = Vec::new();
    let mut offset = 0u64;
    // One scratch buffer for the whole file; each block slices into it.
    let mut buf = vec![0u8; opts.block];
    while offset < size {
        let len = std::cmp::min(opts.block as u64, size - offset) as usize;
        gaps.extend(salvage_range(
            reader,
            writer,
            offset,
            &mut buf[..len],
            opts,
            &mut recovered,
        ));
        offset += len as u64;
    }
    (merge_adjacent(gaps), recovered)
}

/// Re-attempt only the ranges a previous run mapped. Returns the gaps
/// that are still unreadable.
pub fn salvage_gaps(
    reader: &mut dyn BlockReader,
    writer: &mut dyn BlockWriter,
    gaps: &[Gap],
    opts: &SalvageOptions,
) -> (Vec<Gap>, u64) {
    let mut recovered = 0u64;
    let mut remaining = Vec::new();
    let mut buf = vec![0u8; opts.block];
    for gap in gaps {
        // A gap can be larger than one block (merged), so walk it.
        let mut offset = gap.offset;
        let end = gap.offset + gap.len;
        while offset < end {
            let len = std::cmp::min(opts.block as u64, end - offset) as usize;
            remaining.extend(salvage_range(
                reader,
                writer,
                offset,
                &mut buf[..len],
                opts,
                &mut recovered,
            ));
            offset += len as u64;
        }
    }
    (merge_adjacent(remaining), recovered)
}

/// Where the gap manifest for `dst` lives.
fn manifest_path_for(dst: &Path) -> PathBuf {
    with_suffix(dst, MANIFEST_SUFFIX)
}

/// What one salvage run produced.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SalvageReportDto {
    pub source: String,
    pub destination: String,
    pub manifest_path: String,
    pub size: u64,
    pub recovered_bytes: u64,
    /// Bytes still unreadable after this run.
    pub missing_bytes: u64,
    pub gaps: Vec<Gap>,
    /// `true` when this run only re-attempted a prior run's gaps.
    pub resumed: bool,
    /// `true` when nothing is missing — the file is byte-complete.
    pub complete: bool,
}

/// Salvage `src` to `dst`, resuming from an existing gap manifest when
/// one is present and still matches the source size.
#[tauri::command]
pub async fn salvage_copy(
    src: String,
    dst: String,
    state: State<'_, AppState>,
) -> Result<SalvageReportDto, String> {
    let src_path = validate_ipc_path(&src).map_err(|e| e.to_string())?;
    let dst_path = validate_ipc_path(&dst).map_err(|e| e.to_string())?;

    let worker_src = src_path.clone();
    let worker_dst = dst_path.clone();
    let report = tauri::async_runtime::spawn_blocking(move || {
        run_salvage(&worker_src, &worker_dst, &SalvageOptions::default())
    })
    .await
    .map_err(|e| format!("salvage task failed: {e}"))??;

    record_history(&state, &src_path, &dst_path, &report).await;
    Ok(report)
}

/// The blocking body: open both files, decide fresh-vs-resume, run the
/// ladder, write the manifest.
fn run_salvage(src: &Path, dst: &Path, opts: &SalvageOptions) -> Result<SalvageReportDto, String> {
    let mut reader = std::fs::File::open(src).map_err(|e| format!("{}: {e}", src.display()))?;
    let size = reader
        .metadata()
        .map_err(|e| format!("{}: {e}", src.display()))?
        .len();

    let manifest_path = manifest_path_for(dst);
    // Resume only when the manifest matches this source size *and* the
    // destination is already the right length — otherwise the recorded
    // offsets describe a different file and re-attempting them would
    // write recovered bytes to the wrong places.
    let prior = std::fs::read_to_string(&manifest_path)
        .ok()
        .and_then(|text| serde_json::from_str::<SalvageManifest>(&text).ok())
        .filter(|m| m.size == size)
        .filter(|_| {
            std::fs::metadata(dst)
                .map(|m| m.len() == size)
                .unwrap_or(false)
        });

    let mut writer = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(prior.is_none())
        .open(dst)
        .map_err(|e| format!("{}: {e}", dst.display()))?;
    // A fresh run pre-sizes the destination so a gap near the end still
    // leaves the file at its true length.
    if prior.is_none() {
        writer
            .set_len(size)
            .map_err(|e| format!("{}: {e}", dst.display()))?;
    }

    let resumed = prior.is_some();
    let (gaps, recovered_now) = match &prior {
        Some(m) => salvage_gaps(&mut reader, &mut writer, &m.gaps, opts),
        None => salvage_whole(&mut reader, &mut writer, size, opts),
    };
    writer
        .flush()
        .map_err(|e| format!("{}: {e}", dst.display()))?;

    let missing_bytes: u64 = gaps.iter().map(|g| g.len).sum();
    let recovered_bytes = match &prior {
        // A resume adds to what the first pass already recovered.
        Some(m) => m.recovered_bytes + recovered_now,
        None => recovered_now,
    };

    let manifest = SalvageManifest {
        source: src.to_string_lossy().into_owned(),
        destination: dst.to_string_lossy().into_owned(),
        size,
        gaps: gaps.clone(),
        recovered_bytes,
        updated_at_ms: now_ms(),
    };
    // A complete recovery drops the manifest — leaving a gap file
    // claiming zero gaps beside a good copy is just litter.
    if gaps.is_empty() {
        let _ = std::fs::remove_file(&manifest_path);
    } else {
        let body = serde_json::to_string_pretty(&manifest).map_err(|e| e.to_string())?;
        std::fs::write(&manifest_path, body)
            .map_err(|e| format!("{}: {e}", manifest_path.display()))?;
    }

    Ok(SalvageReportDto {
        source: src.to_string_lossy().into_owned(),
        destination: dst.to_string_lossy().into_owned(),
        manifest_path: manifest_path.to_string_lossy().into_owned(),
        size,
        recovered_bytes,
        missing_bytes,
        gaps,
        resumed,
        complete: missing_bytes == 0,
    })
}

async fn record_history(state: &AppState, src: &Path, dst: &Path, report: &SalvageReportDto) {
    let Some(history) = state.history.clone() else {
        return;
    };
    if let Ok(row) = history
        .record_start(&JobSummary {
            kind: "salvage".to_string(),
            status: "running".to_string(),
            src_root: src.to_path_buf(),
            dst_root: dst.to_path_buf(),
            total_bytes: report.recovered_bytes,
            ..Default::default()
        })
        .await
    {
        let _ = history
            .record_item(&ItemRow {
                job_row_id: row.0,
                src: src.to_path_buf(),
                dst: dst.to_path_buf(),
                size: report.recovered_bytes,
                // Salvaged output is deliberately not byte-exact, so it
                // must never read as a clean `ok` in a report.
                status: if report.complete { "ok" } else { "salvaged" }.to_string(),
                hash_hex: None,
                error_code: (!report.complete).then(|| "unreadable-ranges".to_string()),
                error_msg: (!report.complete).then(|| {
                    format!(
                        "{} byte(s) unreadable across {} gap(s)",
                        report.missing_bytes,
                        report.gaps.len()
                    )
                }),
                timestamp_ms: 0,
            })
            .await;
        let _ = history
            .record_finish(
                row,
                if report.complete {
                    "succeeded"
                } else {
                    "failed"
                },
                report.recovered_bytes,
                1,
                u64::from(!report.complete),
            )
            .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A source that fails every read overlapping a poisoned range —
    /// the shape a dying sector actually presents.
    struct FaultyDisk {
        data: Vec<u8>,
        poisoned: Vec<(u64, u64)>,
        reads: usize,
    }

    impl FaultyDisk {
        fn new(data: Vec<u8>, poisoned: Vec<(u64, u64)>) -> Self {
            Self {
                data,
                poisoned,
                reads: 0,
            }
        }

        fn overlaps_bad(&self, offset: u64, len: u64) -> bool {
            self.poisoned
                .iter()
                .any(|(start, plen)| offset < start + plen && *start < offset + len)
        }
    }

    impl BlockReader for FaultyDisk {
        fn read_exact_at(&mut self, offset: u64, buf: &mut [u8]) -> std::io::Result<()> {
            self.reads += 1;
            if self.overlaps_bad(offset, buf.len() as u64) {
                return Err(std::io::Error::other("simulated read failure"));
            }
            let start = offset as usize;
            buf.copy_from_slice(&self.data[start..start + buf.len()]);
            Ok(())
        }
    }

    /// An in-memory destination.
    struct MemSink(Vec<u8>);

    impl BlockWriter for MemSink {
        fn write_all_at(&mut self, offset: u64, buf: &[u8]) -> std::io::Result<()> {
            let start = offset as usize;
            self.0[start..start + buf.len()].copy_from_slice(buf);
            Ok(())
        }
    }

    fn opts() -> SalvageOptions {
        // 64-byte blocks descending to a 16-byte floor keeps the test
        // arithmetic legible while exercising two halvings.
        SalvageOptions {
            block: 64,
            min_block: 16,
            retries: 1,
        }
    }

    #[test]
    fn healthy_media_is_copied_whole_with_no_gaps() {
        let data: Vec<u8> = (0..256u32).map(|i| i as u8).collect();
        let mut disk = FaultyDisk::new(data.clone(), Vec::new());
        let mut sink = MemSink(vec![0; data.len()]);
        let (gaps, recovered) = salvage_whole(&mut disk, &mut sink, data.len() as u64, &opts());
        assert!(gaps.is_empty());
        assert_eq!(recovered, data.len() as u64);
        assert_eq!(sink.0, data);
    }

    #[test]
    fn one_bad_sector_costs_only_a_floor_sized_hole() {
        let data: Vec<u8> = (0..256u32).map(|i| (i + 1) as u8).collect();
        // A single bad byte at offset 70 sits in the second 64-byte
        // block; the ladder must isolate it to one 16-byte gap.
        let mut disk = FaultyDisk::new(data.clone(), vec![(70, 1)]);
        let mut sink = MemSink(vec![0; data.len()]);
        let (gaps, recovered) = salvage_whole(&mut disk, &mut sink, data.len() as u64, &opts());
        assert_eq!(
            gaps,
            vec![Gap {
                offset: 64,
                len: 16
            }]
        );
        assert_eq!(recovered, 256 - 16);
        // Everything outside the gap is byte-exact...
        assert_eq!(sink.0[..64], data[..64]);
        assert_eq!(sink.0[80..], data[80..]);
        // ...and the gap is zero-filled so later bytes keep their
        // offsets rather than sliding forward.
        assert!(sink.0[64..80].iter().all(|b| *b == 0));
    }

    #[test]
    fn adjacent_bad_ranges_merge_into_one_gap() {
        let data: Vec<u8> = vec![7; 128];
        // Poison the whole first block: every descent fails, so the
        // four floor-sized holes must collapse into one entry.
        let mut disk = FaultyDisk::new(data.clone(), vec![(0, 64)]);
        let mut sink = MemSink(vec![0; data.len()]);
        let (gaps, recovered) = salvage_whole(&mut disk, &mut sink, data.len() as u64, &opts());
        assert_eq!(gaps, vec![Gap { offset: 0, len: 64 }]);
        assert_eq!(recovered, 64);
    }

    #[test]
    fn a_transient_failure_inside_the_retry_budget_still_recovers() {
        struct FlakyOnce {
            data: Vec<u8>,
            failed_yet: bool,
        }
        impl BlockReader for FlakyOnce {
            fn read_exact_at(&mut self, offset: u64, buf: &mut [u8]) -> std::io::Result<()> {
                if !self.failed_yet {
                    self.failed_yet = true;
                    return Err(std::io::Error::other("transient"));
                }
                let start = offset as usize;
                buf.copy_from_slice(&self.data[start..start + buf.len()]);
                Ok(())
            }
        }
        let data: Vec<u8> = vec![3; 64];
        let mut disk = FlakyOnce {
            data: data.clone(),
            failed_yet: false,
        };
        let mut sink = MemSink(vec![0; data.len()]);
        let (gaps, recovered) = salvage_whole(&mut disk, &mut sink, 64, &opts());
        assert!(gaps.is_empty(), "one retry should have absorbed it");
        assert_eq!(recovered, 64);
    }

    #[test]
    fn a_resume_reads_only_the_gaps() {
        let data: Vec<u8> = (0..256u32).map(|i| i as u8).collect();
        let mut disk = FaultyDisk::new(data.clone(), vec![(70, 1)]);
        let mut sink = MemSink(vec![0; data.len()]);
        let (gaps, _) = salvage_whole(&mut disk, &mut sink, data.len() as u64, &opts());
        assert_eq!(gaps.len(), 1);

        // Second pass against media that has since healed.
        let mut healed = FaultyDisk::new(data.clone(), Vec::new());
        let (remaining, recovered) = salvage_gaps(&mut healed, &mut sink, &gaps, &opts());
        assert!(remaining.is_empty());
        assert_eq!(recovered, 16, "only the gap was re-read");
        assert_eq!(healed.reads, 1, "exactly one read, not a whole-file pass");
        assert_eq!(sink.0, data, "the file is now byte-exact");
    }

    #[test]
    fn a_resume_that_still_fails_keeps_the_gap() {
        let data: Vec<u8> = vec![9; 128];
        let mut disk = FaultyDisk::new(data.clone(), vec![(0, 16)]);
        let mut sink = MemSink(vec![0; data.len()]);
        let (gaps, _) = salvage_whole(&mut disk, &mut sink, data.len() as u64, &opts());
        let mut still_bad = FaultyDisk::new(data, vec![(0, 16)]);
        let (remaining, recovered) = salvage_gaps(&mut still_bad, &mut sink, &gaps, &opts());
        assert_eq!(remaining, gaps);
        assert_eq!(recovered, 0);
    }

    #[test]
    fn merge_adjacent_leaves_separated_gaps_alone() {
        let gaps = vec![
            Gap { offset: 0, len: 16 },
            Gap {
                offset: 16,
                len: 16,
            },
            Gap {
                offset: 64,
                len: 16,
            },
        ];
        assert_eq!(
            merge_adjacent(gaps),
            vec![
                Gap { offset: 0, len: 32 },
                Gap {
                    offset: 64,
                    len: 16
                }
            ]
        );
    }

    #[test]
    fn manifest_sits_beside_the_destination() {
        assert_eq!(
            manifest_path_for(Path::new("/out/disc.iso")),
            PathBuf::from("/out/disc.iso.freally-salvage.json")
        );
    }

    #[test]
    fn a_healthy_file_salvages_clean_and_leaves_no_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("good.bin");
        let dst = dir.path().join("out.bin");
        let data: Vec<u8> = (0..5000u32).map(|i| i as u8).collect();
        std::fs::write(&src, &data).unwrap();

        let report = run_salvage(&src, &dst, &SalvageOptions::default()).unwrap();
        assert!(report.complete);
        assert_eq!(report.missing_bytes, 0);
        assert_eq!(report.recovered_bytes, data.len() as u64);
        assert!(!report.resumed);
        assert_eq!(std::fs::read(&dst).unwrap(), data);
        assert!(
            !Path::new(&report.manifest_path).exists(),
            "a clean recovery must not litter a gap manifest"
        );
    }

    #[test]
    fn a_destination_of_the_wrong_size_forces_a_full_pass() {
        // A stale manifest whose recorded size disagrees with the
        // source must not be trusted — its offsets describe another
        // file. The run has to start over rather than write recovered
        // bytes into the wrong places.
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src.bin");
        let dst = dir.path().join("dst.bin");
        std::fs::write(&src, vec![1u8; 100]).unwrap();
        std::fs::write(&dst, vec![0u8; 40]).unwrap();
        let manifest = SalvageManifest {
            source: src.to_string_lossy().into_owned(),
            destination: dst.to_string_lossy().into_owned(),
            size: 999,
            gaps: vec![Gap { offset: 0, len: 8 }],
            recovered_bytes: 0,
            updated_at_ms: 0,
        };
        std::fs::write(
            manifest_path_for(&dst),
            serde_json::to_string(&manifest).unwrap(),
        )
        .unwrap();

        let report = run_salvage(&src, &dst, &SalvageOptions::default()).unwrap();
        assert!(!report.resumed);
        assert!(report.complete);
        assert_eq!(std::fs::read(&dst).unwrap(), vec![1u8; 100]);
    }
}
