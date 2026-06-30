//! Phase 50 — Borg importer smoke test.
//!
//! Runs the `copythat_chunk::migrate` Borg importer against a committed,
//! PII-free **repokey** Borg fixture (`tests/fixtures/borg-repo`, created
//! with `borgbackup 1.4.4` inside Linux, passphrase `testpass`) and
//! asserts every file restores byte-for-byte against the originals in
//! `tests/fixtures/borg-src`.
//!
//! No `borg` binary is required — the importer reads the encrypted
//! segment log directly (PBKDF2-HMAC-SHA256 key unwrap + AES-256-CTR +
//! HMAC-SHA256 + hand-rolled MessagePack / LZ4), so this validates the
//! whole pipeline in CI on every platform.

use std::path::PathBuf;

use copythat_chunk::{MigrateError, RepoFormat, Repository, SnapshotId, materialise_file, migrate};

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures")
}

#[test]
fn case1_import_borg_fixture_restores_byte_identical() {
    let repo = fixtures().join("borg-repo");
    let src = fixtures().join("borg-src");
    let tmp = tempfile::tempdir().unwrap();
    let cdr = tmp.path().join("cdr");

    let report = migrate(RepoFormat::Borg, &repo, &cdr, Some("testpass")).unwrap();
    assert!(
        report.snapshots >= 1,
        "expected >=1 archive, got {}",
        report.snapshots
    );
    assert_eq!(report.files, 2, "expected 2 files (hello.txt + big.txt)");

    let dest = Repository::open(&cdr).unwrap();
    let summaries = dest.snapshots().unwrap();
    assert!(!summaries.is_empty());
    let snap = dest.snapshot(SnapshotId(summaries[0].id)).unwrap().unwrap();

    let mut verified = 0;
    for fe in &snap.files {
        let name = fe.path.rsplit('/').next().unwrap_or("");
        let expected = src.join(name);
        if expected.is_file() {
            let out = tmp.path().join(format!("out-{name}"));
            materialise_file(dest.store(), &fe.manifest, &out).unwrap();
            assert_eq!(
                std::fs::read(&out).unwrap(),
                std::fs::read(&expected).unwrap(),
                "restored {name} must match the original byte-for-byte",
            );
            verified += 1;
        }
    }
    assert_eq!(verified, 2, "expected to verify hello.txt + big.txt");
}

#[test]
fn case2_wrong_passphrase_rejected() {
    let repo = fixtures().join("borg-repo");
    let tmp = tempfile::tempdir().unwrap();
    let err = migrate(
        RepoFormat::Borg,
        &repo,
        &tmp.path().join("cdr"),
        Some("wrong"),
    )
    .expect_err("wrong passphrase must fail");
    let msg = format!("{err}").to_lowercase();
    assert!(
        msg.contains("passphrase") || msg.contains("hmac") || msg.contains("decrypt"),
        "expected a passphrase/HMAC error, got: {err}"
    );
}

#[test]
fn case3_missing_passphrase_is_typed() {
    let repo = fixtures().join("borg-repo");
    let tmp = tempfile::tempdir().unwrap();
    let err = migrate(RepoFormat::Borg, &repo, &tmp.path().join("cdr"), None)
        .expect_err("missing passphrase must fail");
    assert!(matches!(
        err,
        MigrateError::NeedsPassphrase { tool: "borg" }
    ));
}
