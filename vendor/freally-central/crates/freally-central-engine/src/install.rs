// Silent-install engine (Phase 5 / FC-40..42).
//
// Runs verified installers unattended, accepting each installer's defaults:
// Windows NSIS `/S` (MSI `msiexec /qn` as fallback), macOS .dmg mount + copy
// into Applications, Linux AppImage placement or deb/rpm via a single pkexec.
// Install progress streams over the same IPC-channel pattern as downloads, as
// a determinate staged estimate that only claims 100% on real success (FC-41).
//
// Trust model (the Phase 4 security carry-over): the digest the download
// engine verified came from the same GitHub API response as the URL, so the
// catalog manifest was the sole trust anchor — fine for downloading, not for
// executing. Before ANY file runs, this engine adds checks the manifest cannot
// influence:
//   1. The UI never names a file — it names an app id, resolved through the
//      VerifiedDownloads registry that only the download pipeline writes.
//   2. `checksum_verified` must be true: no published digest → no silent run.
//   3. The source URL's owner must be in TRUSTED_OWNERS, a list compiled into
//      this binary — a tampered manifest pointing at a foreign repo is refused.
//   4. The file is re-hashed on disk and must still match the verified SHA-256
//      (closing the window between download and execute); a mismatch deletes
//      the file and refuses.
// Failures are reported per app with stable codes, never dressed up as success.
//
// Elevation (FC-40): at most one consent per batch. The preferred installer on
// every OS needs none (per-user NSIS, Applications copy, AppImage); Linux
// deb/rpm installs are grouped into one pkexec invocation for the whole batch.

use std::collections::HashSet;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tauri::ipc::Channel;
use tauri::State;

use crate::download::{VerifiedDownloads, VerifiedFile};

/// GitHub owners whose release assets Central may execute. Compiled into the
/// binary on purpose: the catalog manifest is fetched remotely and must never
/// be able to widen this list.
const TRUSTED_OWNERS: [&str; 1] = ["mikesruthless12"];

/// One app to install; the id resolves to a verified file server-side.
#[derive(Debug, Clone, Deserialize)]
pub struct InstallRequest {
    pub id: String,
}

/// Per-app install events streamed to the UI. Every requested id receives
/// exactly one terminal event (`installed` / `failed` / `canceled`).
#[derive(Debug, Clone, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum InstallEvent {
    /// This app's installer is starting its verification + run.
    Started { id: String },
    /// Staged determinate estimate in [0, 1] (FC-41) — capped at 0.90 while
    /// the installer runs; 1.0 is only ever emitted on real success.
    Progress { id: String, fraction: f64 },
    /// The installer completed successfully.
    Installed { id: String },
    /// The install failed or was refused. `code` is a stable machine code the
    /// UI maps to a localized message; `detail` is for logs only.
    Failed {
        id: String,
        code: &'static str,
        detail: String,
    },
    /// Canceled before its installer started (a running installer is never
    /// killed mid-run — that could corrupt an install).
    Canceled { id: String },
}

/// Stable failure codes (the UI maps these to localized messages).
mod fail {
    pub const NOT_DOWNLOADED: &str = "notDownloaded";
    pub const NO_CHECKSUM: &str = "noChecksum";
    pub const UNTRUSTED_SOURCE: &str = "untrustedSource";
    pub const TAMPERED: &str = "tampered";
    pub const UNSUPPORTED_INSTALLER: &str = "unsupportedInstaller";
    pub const INSTALLER_FAILED: &str = "installerFailed";
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub const ELEVATION_DECLINED: &str = "elevationDeclined";
    pub const BUSY: &str = "busy";
    pub const IO: &str = "io";
}

/// The one running install batch's cancel flag (None = no batch running).
/// Installers must never run concurrently — Windows Installer serializes MSIs
/// anyway, and sequential runs keep progress and failures attributable.
#[derive(Default)]
pub struct Installs(Mutex<Option<Arc<AtomicBool>>>);

/// Frees the batch slot on every exit path (including panics).
struct BatchSlot<'a>(&'a Installs);

impl Drop for BatchSlot<'_> {
    fn drop(&mut self) {
        if let Ok(mut slot) = self.0 .0.lock() {
            *slot = None;
        }
    }
}

type Fail = (&'static str, String);

/// Install every requested app, sequentially, streaming per-app events.
///
/// The returned future resolves when the whole batch is terminal; outcomes are
/// always delivered as events (the UI drives its state from the channel alone).
#[tauri::command]
pub async fn central_install_apps(
    requests: Vec<InstallRequest>,
    on_event: Channel<InstallEvent>,
    installs: State<'_, Installs>,
    verified: State<'_, VerifiedDownloads>,
) -> Result<(), String> {
    // Claim the single batch slot; a second batch is refused per app, honestly,
    // not queued invisibly behind the first.
    let cancel = Arc::new(AtomicBool::new(false));
    {
        let mut slot = installs
            .0
            .lock()
            .map_err(|_| "install registry poisoned".to_string())?;
        if slot.is_some() {
            for request in &requests {
                failed(
                    &on_event,
                    &request.id,
                    fail::BUSY,
                    "an install batch is already running",
                );
            }
            return Ok(());
        }
        *slot = Some(Arc::clone(&cancel));
    }
    let _slot = BatchSlot(&installs);

    // Dedupe ids so no installer can be run twice by one request.
    let mut seen = HashSet::new();
    let requests: Vec<InstallRequest> = requests
        .into_iter()
        .filter(|r| seen.insert(r.id.clone()))
        .collect();

    run_batch(&requests, &cancel, &on_event, &verified).await;
    Ok(())
}

/// Cancel the running install batch: apps whose installer has not started yet
/// end as `canceled`; a currently running installer finishes and reports its
/// real outcome (killing an installer mid-run can corrupt an install).
#[tauri::command]
pub fn central_cancel_installs(installs: State<'_, Installs>) {
    if let Ok(slot) = installs.0.lock() {
        if let Some(flag) = slot.as_ref() {
            flag.store(true, Ordering::Relaxed);
        }
    }
}

fn emit(channel: &Channel<InstallEvent>, event: InstallEvent) {
    let _ = channel.send(event);
}

fn progress(channel: &Channel<InstallEvent>, id: &str, fraction: f64) {
    emit(
        channel,
        InstallEvent::Progress {
            id: id.to_string(),
            fraction,
        },
    );
}

fn failed(
    channel: &Channel<InstallEvent>,
    id: &str,
    code: &'static str,
    detail: impl Into<String>,
) {
    emit(
        channel,
        InstallEvent::Failed {
            id: id.to_string(),
            code,
            detail: detail.into(),
        },
    );
}

fn canceled(channel: &Channel<InstallEvent>, id: &str) {
    emit(channel, InstallEvent::Canceled { id: id.to_string() });
}

fn io_fail(e: impl std::fmt::Display) -> Fail {
    (fail::IO, e.to_string())
}

/// What a file name says the installer is; how to run it silently per OS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InstallerKind {
    NsisExe,
    Msi,
    Dmg,
    AppImage,
    Deb,
    Rpm,
}

fn installer_kind(file_name: &str) -> Option<InstallerKind> {
    let lower = file_name.to_ascii_lowercase();
    if lower.ends_with(".exe") {
        Some(InstallerKind::NsisExe)
    } else if lower.ends_with(".msi") {
        Some(InstallerKind::Msi)
    } else if lower.ends_with(".dmg") {
        Some(InstallerKind::Dmg)
    } else if lower.ends_with(".appimage") {
        Some(InstallerKind::AppImage)
    } else if lower.ends_with(".deb") {
        Some(InstallerKind::Deb)
    } else if lower.ends_with(".rpm") {
        Some(InstallerKind::Rpm)
    } else {
        None
    }
}

/// Whether this build of Central can run the installer kind on this OS.
fn supported_here(kind: InstallerKind) -> bool {
    match kind {
        InstallerKind::NsisExe | InstallerKind::Msi => cfg!(windows),
        InstallerKind::Dmg => cfg!(target_os = "macos"),
        InstallerKind::AppImage | InstallerKind::Deb | InstallerKind::Rpm => {
            cfg!(target_os = "linux")
        }
    }
}

/// True for kinds that need root and therefore join the single elevated group
/// call (Linux deb/rpm); everything else installs without elevation.
fn needs_group_elevation(kind: InstallerKind) -> bool {
    matches!(kind, InstallerKind::Deb | InstallerKind::Rpm)
}

/// The `<owner>` segment of a github.com release-asset URL, lowercased.
/// (GitHub logins are case-insensitive.) Re-validates the full asset shape
/// with the download engine's own validator — the gate must not trust a
/// github.com URL on owner alone, even though the registry only ever records
/// already-validated URLs.
fn owner_of_asset_url(url: &str) -> Option<String> {
    let url = crate::download::validate_asset_url(url).ok()?;
    let mut segments = url.path_segments()?.filter(|s| !s.is_empty());
    segments.next().map(|owner| owner.to_ascii_lowercase())
}

fn trusted_owner(url: &str) -> bool {
    owner_of_asset_url(url).is_some_and(|owner| TRUSTED_OWNERS.contains(&owner.as_str()))
}

/// One app that passed the trust gate and is cleared to run.
struct Plan {
    id: String,
    file: VerifiedFile,
    kind: InstallerKind,
}

/// Resolve an app id through the trust gate (steps 1–3 of the model above).
fn resolve_plan(id: &str, verified: &VerifiedDownloads) -> Result<Plan, Fail> {
    let file = verified.get(id).ok_or_else(|| {
        (
            fail::NOT_DOWNLOADED,
            format!("no verified download for {id} this session"),
        )
    })?;
    let kind = installer_kind(&file.file_name).ok_or_else(|| {
        (
            fail::UNSUPPORTED_INSTALLER,
            format!("{} is not a runnable installer", file.file_name),
        )
    })?;
    if !supported_here(kind) {
        return Err((
            fail::UNSUPPORTED_INSTALLER,
            format!("{} cannot be installed on this OS", file.file_name),
        ));
    }
    if !file.checksum_verified {
        return Err((
            fail::NO_CHECKSUM,
            "no published checksum — silent install requires a verified digest".into(),
        ));
    }
    if !trusted_owner(&file.source_url) {
        return Err((
            fail::UNTRUSTED_SOURCE,
            format!("{} is not from a trusted release owner", file.source_url),
        ));
    }
    Ok(Plan {
        id: id.to_string(),
        file,
        kind,
    })
}

async fn run_batch(
    requests: &[InstallRequest],
    cancel: &AtomicBool,
    on_event: &Channel<InstallEvent>,
    verified: &VerifiedDownloads,
) {
    // Gate every id first — refusals are terminal immediately, and only cleared
    // plans continue toward a process spawn.
    let mut plans: Vec<Plan> = Vec::new();
    for request in requests {
        match resolve_plan(&request.id, verified) {
            Ok(plan) => plans.push(plan),
            Err((code, detail)) => failed(on_event, &request.id, code, detail),
        }
    }

    let (group, solo): (Vec<Plan>, Vec<Plan>) = plans
        .into_iter()
        .partition(|p| needs_group_elevation(p.kind));

    for plan in solo {
        if cancel.load(Ordering::Relaxed) {
            canceled(on_event, &plan.id);
            continue;
        }
        install_one(&plan, cancel, on_event, verified).await;
    }

    if !group.is_empty() {
        install_group(group, cancel, on_event, verified).await;
    }
}

/// The steps shared by solo and grouped installs before anything runs:
/// `started` event, then the on-disk re-hash (trust-gate step 4).
/// Ok(true) = cleared to run; Ok(false) = canceled (event already emitted).
async fn preflight(
    plan: &Plan,
    cancel: &AtomicBool,
    on_event: &Channel<InstallEvent>,
    verified: &VerifiedDownloads,
) -> Result<bool, Fail> {
    emit(
        on_event,
        InstallEvent::Started {
            id: plan.id.clone(),
        },
    );
    progress(on_event, &plan.id, 0.05);

    let actual = sha256_of_file(&plan.file.path).await.map_err(io_fail)?;
    if actual != plan.file.sha256 {
        // The bytes changed since verification — refuse, and make sure neither
        // the file nor its registry entry can be tried again.
        verified.remove(&plan.id);
        let _ = tokio::fs::remove_file(&plan.file.path).await;
        return Err((
            fail::TAMPERED,
            "on-disk installer no longer matches its verified checksum".into(),
        ));
    }
    progress(on_event, &plan.id, 0.25);

    if cancel.load(Ordering::Relaxed) {
        canceled(on_event, &plan.id);
        return Ok(false);
    }
    Ok(true)
}

async fn install_one(
    plan: &Plan,
    cancel: &AtomicBool,
    on_event: &Channel<InstallEvent>,
    verified: &VerifiedDownloads,
) {
    let outcome = match preflight(plan, cancel, on_event, verified).await {
        Ok(true) => run_installer(plan, on_event).await,
        Ok(false) => return, // canceled — terminal event already emitted
        Err(failure) => Err(failure),
    };
    match outcome {
        Ok(()) => {
            progress(on_event, &plan.id, 1.0);
            emit(
                on_event,
                InstallEvent::Installed {
                    id: plan.id.clone(),
                },
            );
        }
        Err((code, detail)) => failed(on_event, &plan.id, code, detail),
    }
}

/// Spawn the command and wait for it, emitting the staged progress estimate
/// for `ids` while it runs. The fraction climbs from `base` toward a 0.90 cap
/// (half of the remaining distance every ~10 s) — the last 10% is only ever
/// claimed by real success.
async fn wait_with_ramp(
    command: &mut tokio::process::Command,
    ids: &[String],
    base: f64,
    on_event: &Channel<InstallEvent>,
) -> std::io::Result<std::process::ExitStatus> {
    let mut child = command.spawn()?;
    let started = Instant::now();
    let mut interval = tokio::time::interval(Duration::from_millis(500));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            status = child.wait() => return status,
            _ = interval.tick() => {
                let fraction = ramp_fraction(base, started.elapsed());
                for id in ids {
                    progress(on_event, id, fraction);
                }
            }
        }
    }
}

/// The determinate staged estimate between spawn and exit (FC-41): monotonic,
/// asymptotic to the 0.90 cap, never claiming completion.
fn ramp_fraction(base: f64, elapsed: Duration) -> f64 {
    let t = elapsed.as_secs_f64();
    (base + (0.90 - base) * (t / (t + 10.0))).min(0.90)
}

/// SHA-256 of a file's current on-disk bytes (the download engine's own
/// buffered hash loop — one implementation to keep correct).
async fn sha256_of_file(path: &Path) -> std::io::Result<String> {
    let mut file = tokio::fs::File::open(path).await?;
    let mut hasher = Sha256::new();
    crate::download::hash_file_into(&mut file, &mut hasher).await?;
    Ok(hex::encode(hasher.finalize()))
}

// --- Windows ---------------------------------------------------------------------

#[cfg(windows)]
async fn run_installer(plan: &Plan, on_event: &Channel<InstallEvent>) -> Result<(), Fail> {
    let mut command = match plan.kind {
        // Tauri's NSIS installer is per-user by default, so /S runs with no
        // wizard and no elevation prompt.
        InstallerKind::NsisExe => {
            let mut c = tokio::process::Command::new(&plan.file.path);
            c.arg("/S");
            c
        }
        // MSI fallback (pickInstaller prefers the .exe): /qn is fully silent;
        // a per-machine MSI may still raise one UAC consent.
        InstallerKind::Msi => {
            let mut c = tokio::process::Command::new("msiexec");
            c.arg("/i").arg(&plan.file.path).args(["/qn", "/norestart"]);
            c
        }
        _ => {
            return Err((
                fail::UNSUPPORTED_INSTALLER,
                format!("{:?} is not a Windows installer", plan.kind),
            ))
        }
    };
    let status = wait_with_ramp(&mut command, std::slice::from_ref(&plan.id), 0.25, on_event)
        .await
        .map_err(io_fail)?;
    // 3010 = ERROR_SUCCESS_REBOOT_REQUIRED: the install itself succeeded.
    if status.success() || status.code() == Some(3010) {
        Ok(())
    } else {
        Err((
            fail::INSTALLER_FAILED,
            format!("installer exited with {status}"),
        ))
    }
}

// --- macOS -----------------------------------------------------------------------

#[cfg(target_os = "macos")]
async fn run_installer(plan: &Plan, on_event: &Channel<InstallEvent>) -> Result<(), Fail> {
    if plan.kind != InstallerKind::Dmg {
        return Err((
            fail::UNSUPPORTED_INSTALLER,
            format!("{:?} is not a macOS installer", plan.kind),
        ));
    }
    // Mount the verified .dmg at a private mountpoint, copy the .app into
    // Applications, detach. This is exactly the drag-install a user would do
    // by hand — no wizard, no elevation.
    let mount = std::env::temp_dir().join(format!("freally-central-mount-{}", plan.id));
    tokio::fs::create_dir_all(&mount).await.map_err(io_fail)?;

    let attach = tokio::process::Command::new("hdiutil")
        .args([
            "attach",
            "-nobrowse",
            "-readonly",
            "-noautoopen",
            "-mountpoint",
        ])
        .arg(&mount)
        .arg(&plan.file.path)
        .status()
        .await
        .map_err(io_fail)?;
    if !attach.success() {
        let _ = tokio::fs::remove_dir_all(&mount).await;
        return Err((
            fail::INSTALLER_FAILED,
            format!("hdiutil attach exited with {attach}"),
        ));
    }
    progress(on_event, &plan.id, 0.40);

    let result = copy_app_from_mount(plan, &mount, on_event).await;

    // Always unmount, even when the copy failed.
    let _ = tokio::process::Command::new("hdiutil")
        .args(["detach", "-force"])
        .arg(&mount)
        .status()
        .await;
    let _ = tokio::fs::remove_dir_all(&mount).await;
    result
}

#[cfg(target_os = "macos")]
async fn copy_app_from_mount(
    plan: &Plan,
    mount: &Path,
    on_event: &Channel<InstallEvent>,
) -> Result<(), Fail> {
    use std::path::PathBuf;

    // The bundle inside the dmg (Tauri's dmg layout: one .app at the root).
    let mut app: Option<(PathBuf, std::ffi::OsString)> = None;
    let mut entries = tokio::fs::read_dir(mount).await.map_err(io_fail)?;
    while let Some(entry) = entries.next_entry().await.map_err(io_fail)? {
        if entry.file_name().to_string_lossy().ends_with(".app") {
            app = Some((entry.path(), entry.file_name()));
            break;
        }
    }
    let (app_src, app_name) = app.ok_or((
        fail::INSTALLER_FAILED,
        "no .app bundle inside the dmg".into(),
    ))?;

    // /Applications first; the user's ~/Applications when it isn't writable —
    // the same probe order detection reads (detect::applications_dirs).
    for dest_dir in crate::detect::applications_dirs() {
        if tokio::fs::create_dir_all(&dest_dir).await.is_err() {
            continue;
        }
        let dest = dest_dir.join(&app_name);
        // Stage the copy beside the destination first: the user's working
        // install is only replaced once the whole new bundle has arrived — a
        // failed copy must never cost them the app they already had.
        let staging = dest_dir.join(format!(
            ".freally-central-staging-{}",
            app_name.to_string_lossy()
        ));
        let _ = tokio::fs::remove_dir_all(&staging).await;
        let mut command = tokio::process::Command::new("ditto");
        command.arg(&app_src).arg(&staging);
        let status = wait_with_ramp(&mut command, std::slice::from_ref(&plan.id), 0.40, on_event)
            .await
            .map_err(io_fail)?;
        if !status.success() {
            let _ = tokio::fs::remove_dir_all(&staging).await;
            continue;
        }
        // The dmg carries the browser-download quarantine flag; Central has
        // already verified authenticity (trusted owner + exact digest, both
        // re-checked above), so clear it from the staged copy — otherwise the
        // hands-off install of the brand's unsigned macOS builds ends in a
        // Gatekeeper block instead of a working app.
        let _ = tokio::process::Command::new("xattr")
            .args(["-dr", "com.apple.quarantine"])
            .arg(&staging)
            .status()
            .await;
        // Swap: replace (never merge — ditto into an existing bundle would mix
        // two versions) only now that the staged bundle is complete. If the old
        // copy can't be removed here, leave it and try the next location.
        let _ = tokio::fs::remove_dir_all(&dest).await;
        if tokio::fs::metadata(&dest).await.is_ok() {
            let _ = tokio::fs::remove_dir_all(&staging).await;
            continue;
        }
        if tokio::fs::rename(&staging, &dest).await.is_ok() {
            return Ok(());
        }
        let _ = tokio::fs::remove_dir_all(&staging).await;
    }
    Err((
        fail::INSTALLER_FAILED,
        "could not copy the app into Applications".into(),
    ))
}

// --- Linux -----------------------------------------------------------------------

#[cfg(target_os = "linux")]
async fn run_installer(plan: &Plan, on_event: &Channel<InstallEvent>) -> Result<(), Fail> {
    // deb/rpm go through install_group; the only solo Linux kind is AppImage.
    if plan.kind != InstallerKind::AppImage {
        return Err((
            fail::UNSUPPORTED_INSTALLER,
            format!("{:?} does not install solo on Linux", plan.kind),
        ));
    }
    install_appimage(plan, on_event).await
}

/// AppImage "install": make it executable and place it in ~/Applications —
/// one of the directories installed-app detection searches, so what this
/// writes is what detection reads back.
#[cfg(target_os = "linux")]
async fn install_appimage(plan: &Plan, on_event: &Channel<InstallEvent>) -> Result<(), Fail> {
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    let home = std::env::var_os("HOME").ok_or((fail::IO, "HOME is not set".to_string()))?;
    let dir = PathBuf::from(home).join("Applications");
    tokio::fs::create_dir_all(&dir).await.map_err(io_fail)?;
    progress(on_event, &plan.id, 0.50);

    // Stage beside the destination, then swap into place: any existing install
    // is only touched once the new file has fully arrived — a failed copy must
    // never cost the user the version they already had. (The .staging suffix
    // keeps the temp file invisible to detection, which only reads *.AppImage.)
    let dest = dir.join(&plan.file.file_name);
    let staging = dir.join(format!("{}.fc-staging", plan.file.file_name));
    let stage = async {
        tokio::fs::copy(&plan.file.path, &staging).await?;
        let mut perms = tokio::fs::metadata(&staging).await?.permissions();
        perms.set_mode(0o755);
        tokio::fs::set_permissions(&staging, perms).await?;
        tokio::fs::rename(&staging, &dest).await
    };
    if let Err(e) = stage.await {
        let _ = tokio::fs::remove_file(&staging).await;
        return Err((fail::IO, e.to_string()));
    }
    progress(on_event, &plan.id, 0.85);

    // Only now drop OLDER copies of this app (same filename prefix before the
    // version token, never the file just placed) so detection can't read a
    // stale version afterwards.
    let prefix = appimage_prefix(&plan.file.file_name);
    if let Ok(mut entries) = tokio::fs::read_dir(&dir).await {
        while let Ok(Some(entry)) = entries.next_entry().await {
            let name = entry.file_name();
            let lower = name.to_string_lossy().to_lowercase();
            if lower.ends_with(".appimage")
                && lower.starts_with(&prefix)
                && name != plan.file.file_name.as_str()
            {
                let _ = tokio::fs::remove_file(entry.path()).await;
            }
        }
    }
    Ok(())
}

/// The lowercased filename prefix before the version token, e.g.
/// "Freally Capture_0.4.0_amd64.AppImage" → "freally capture_".
#[cfg(target_os = "linux")]
fn appimage_prefix(file_name: &str) -> String {
    let lower = file_name.to_lowercase();
    match lower.split_once('_') {
        Some((head, _)) => format!("{head}_"),
        None => lower,
    }
}

/// Install all root-requiring packages (deb/rpm) with a single elevation
/// consent per package format — one pkexec call covers the whole batch.
/// Afterwards each package is post-checked individually (the group exit code
/// alone can't say which package landed), so success is never overstated.
#[cfg(target_os = "linux")]
async fn install_group(
    plans: Vec<Plan>,
    cancel: &AtomicBool,
    on_event: &Channel<InstallEvent>,
    verified: &VerifiedDownloads,
) {
    // Pre-flight everyone first: only re-verified files enter the elevated call.
    let mut ready: Vec<Plan> = Vec::new();
    for plan in plans {
        if cancel.load(Ordering::Relaxed) {
            canceled(on_event, &plan.id);
            continue;
        }
        match preflight(&plan, cancel, on_event, verified).await {
            Ok(true) => ready.push(plan),
            Ok(false) => {} // canceled — event already emitted
            Err((code, detail)) => failed(on_event, &plan.id, code, detail),
        }
    }

    for kind in [InstallerKind::Deb, InstallerKind::Rpm] {
        let subset: Vec<&Plan> = ready.iter().filter(|p| p.kind == kind).collect();
        if subset.is_empty() {
            continue;
        }
        if cancel.load(Ordering::Relaxed) {
            for plan in &subset {
                canceled(on_event, &plan.id);
            }
            continue;
        }

        let mut command = tokio::process::Command::new("pkexec");
        match kind {
            InstallerKind::Deb => {
                command.arg("dpkg").arg("-i");
            }
            _ => {
                command.arg("rpm").arg("-U").arg("--replacepkgs");
            }
        }
        for plan in &subset {
            command.arg(&plan.file.path);
        }
        let ids: Vec<String> = subset.iter().map(|p| p.id.clone()).collect();

        match wait_with_ramp(&mut command, &ids, 0.25, on_event).await {
            Err(e) => {
                let detail = if e.kind() == std::io::ErrorKind::NotFound {
                    "pkexec is not available on this system".to_string()
                } else {
                    e.to_string()
                };
                for plan in &subset {
                    failed(on_event, &plan.id, fail::INSTALLER_FAILED, detail.clone());
                }
            }
            // pkexec: 126 = consent dialog dismissed, 127 = not authorized.
            Ok(status) if matches!(status.code(), Some(126) | Some(127)) => {
                for plan in &subset {
                    failed(
                        on_event,
                        &plan.id,
                        fail::ELEVATION_DECLINED,
                        "administrator authorization was not granted",
                    );
                }
            }
            Ok(status) => {
                for plan in &subset {
                    if package_installed(plan) {
                        progress(on_event, &plan.id, 1.0);
                        emit(
                            on_event,
                            InstallEvent::Installed {
                                id: plan.id.clone(),
                            },
                        );
                    } else {
                        failed(
                            on_event,
                            &plan.id,
                            fail::INSTALLER_FAILED,
                            format!("package not installed (group exit {status})"),
                        );
                    }
                }
            }
        }
    }
}

/// Post-check one grouped package honestly: the package manager must now
/// report it installed, at the version encoded in the package file's own name
/// (when one parses — Tauri encodes it in every deb/rpm it emits).
#[cfg(target_os = "linux")]
fn package_installed(plan: &Plan) -> bool {
    let stem = plan
        .file
        .file_name
        .rsplit_once('.')
        .map_or(plan.file.file_name.as_str(), |(stem, _)| stem);
    let expected = crate::detect::version_token(stem);

    // Tauri's package name is the product name lowercased and hyphenated, which
    // equals the manifest id for the brand's apps; the deb filename prefix is
    // the package name too. Check both, deduped.
    let mut candidates = vec![plan.id.clone()];
    if let Some((prefix, _)) = plan.file.file_name.split_once('_') {
        let prefix = prefix.to_lowercase();
        if prefix != plan.id {
            candidates.push(prefix);
        }
    }

    for pkg in &candidates {
        // Ask only the manager that ran this package's format — a deb never
        // lands in the rpm database, so the other probe is a wasted spawn.
        let version = match plan.kind {
            InstallerKind::Deb => crate::detect::dpkg_version(pkg),
            _ => crate::detect::rpm_version(pkg),
        };
        if let Some(version) = version {
            return match &expected {
                Some(expected) => version == *expected,
                None => true,
            };
        }
    }
    false
}

/// Non-Linux stub: resolve_plan's supported_here() already refused deb/rpm off
/// Linux, so the group is always empty — this exists so run_batch compiles
/// everywhere.
#[cfg(not(target_os = "linux"))]
async fn install_group(
    plans: Vec<Plan>,
    _cancel: &AtomicBool,
    _on_event: &Channel<InstallEvent>,
    _verified: &VerifiedDownloads,
) {
    debug_assert!(plans.is_empty(), "deb/rpm plans are refused off-Linux");
}

// --- Fallback (unsupported targets) ------------------------------------------------

#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
async fn run_installer(_plan: &Plan, _on_event: &Channel<InstallEvent>) -> Result<(), Fail> {
    Err((
        fail::UNSUPPORTED_INSTALLER,
        "silent install is not supported on this platform".into(),
    ))
}

// --- Open an installed app (FC-42) -------------------------------------------------

/// Launch an installed catalog app. `id` and `name` come from the manifest, so
/// they are constrained to plain product tokens, and every OS path below only
/// launches something an installer actually put on this machine — the manifest
/// alone can never turn this into "run an arbitrary command".
#[tauri::command]
pub async fn central_launch_app(id: String, name: String) -> Result<(), String> {
    let id = safe_token(&id, false)?;
    let name = safe_token(&name, true)?;
    tauri::async_runtime::spawn_blocking(move || launch(&id, &name))
        .await
        .map_err(|e| e.to_string())?
}

/// Constrain a manifest-supplied value to a plain product token: alphanumeric
/// start, then alphanumerics, `-`, `_`, `.` (and spaces for product names).
fn safe_token(value: &str, allow_space: bool) -> Result<String, String> {
    let value = value.trim();
    let ok = !value.is_empty()
        && value.len() <= 100
        && value
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphanumeric())
        && value.chars().all(|c| {
            c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') || (allow_space && c == ' ')
        });
    if ok {
        Ok(value.to_string())
    } else {
        Err(format!("invalid app token: {value}"))
    }
}

#[cfg(windows)]
fn launch(_id: &str, name: &str) -> Result<(), String> {
    // The same uninstall entries detection reads (one shared walk — the badge
    // and the Open button can never drift apart): the exe path comes from what
    // the installer recorded (InstallLocation, else DisplayIcon), never a guess.
    let mut outcome: Option<Result<(), String>> = None;
    crate::detect::visit_uninstall_entries(|app, display_name| {
        if display_name != name {
            return true;
        }
        if let Ok(location) = app.get_value::<String, _>("InstallLocation") {
            let exe = Path::new(location.trim()).join(format!("{name}.exe"));
            if exe.is_file() {
                outcome = Some(spawn_detached(&exe));
                return false;
            }
        }
        if let Ok(icon) = app.get_value::<String, _>("DisplayIcon") {
            if let Some(exe) = display_icon_exe(&icon) {
                outcome = Some(spawn_detached(&exe));
                return false;
            }
        }
        true
    });
    outcome.unwrap_or_else(|| Err(format!("{name} is not installed")))
}

/// The existing executable a DisplayIcon value points at, if any: strips
/// quotes and a ",<index>" suffix (the resource index can be negative, ",-1").
#[cfg(windows)]
fn display_icon_exe(icon: &str) -> Option<std::path::PathBuf> {
    let cleaned = icon.trim().trim_matches('"');
    let cleaned = match cleaned.rsplit_once(',') {
        Some((head, index))
            if {
                let digits = index.trim().strip_prefix('-').unwrap_or(index.trim());
                !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit())
            } =>
        {
            head
        }
        _ => cleaned,
    };
    let exe = Path::new(cleaned.trim().trim_matches('"'));
    (exe.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("exe"))
        && exe.is_file())
    .then(|| exe.to_path_buf())
}

#[cfg(windows)]
fn spawn_detached(exe: &Path) -> Result<(), String> {
    std::process::Command::new(exe)
        .current_dir(exe.parent().unwrap_or_else(|| Path::new(".")))
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

#[cfg(target_os = "macos")]
fn launch(_id: &str, name: &str) -> Result<(), String> {
    // `open -a` resolves only installed applications by display name.
    let status = std::process::Command::new("open")
        .arg("-a")
        .arg(name)
        .status()
        .map_err(|e| e.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{name} is not installed"))
    }
}

#[cfg(target_os = "linux")]
fn launch(id: &str, name: &str) -> Result<(), String> {
    // Package-managed install: the binary is the package name (Tauri's deb/rpm
    // layout, the same candidates detection probes). Spawn a name only after
    // dpkg/rpm confirm it IS an installed package — a manifest naming some
    // random binary is refused here.
    for pkg in crate::detect::package_candidates(name, id) {
        let installed = crate::detect::dpkg_version(&pkg).is_some()
            || crate::detect::rpm_version(&pkg).is_some();
        if installed && std::process::Command::new(&pkg).spawn().is_ok() {
            return Ok(());
        }
    }
    // AppImage install: the exact file detection would report.
    if let Some((appimage, _)) = crate::detect::find_appimage(name, id) {
        if std::process::Command::new(&appimage).spawn().is_ok() {
            return Ok(());
        }
    }
    Err(format!("{name} is not installed"))
}

#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
fn launch(_id: &str, _name: &str) -> Result<(), String> {
    Err("launching apps is not supported on this platform".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installer_kind_maps_known_extensions_only() {
        assert_eq!(
            installer_kind("Freally-Capture_1.0.0_x64-setup.exe"),
            Some(InstallerKind::NsisExe)
        );
        assert_eq!(
            installer_kind("Freally-Capture_1.0.0_x64.MSI"),
            Some(InstallerKind::Msi)
        );
        assert_eq!(
            installer_kind("Freally Capture_1.0.0_aarch64.dmg"),
            Some(InstallerKind::Dmg)
        );
        assert_eq!(
            installer_kind("Freally Capture_1.0.0_amd64.AppImage"),
            Some(InstallerKind::AppImage)
        );
        assert_eq!(
            installer_kind("Freally Capture_1.0.0_amd64.deb"),
            Some(InstallerKind::Deb)
        );
        assert_eq!(
            installer_kind("Freally Capture-1.0.0-1.x86_64.rpm"),
            Some(InstallerKind::Rpm)
        );
        // Non-installer release assets can never become runnable.
        assert_eq!(installer_kind("latest.json"), None);
        assert_eq!(installer_kind("app.exe.sig"), None);
        assert_eq!(installer_kind("app.app.tar.gz"), None);
    }

    #[test]
    fn only_allowlisted_owners_are_trusted() {
        assert!(trusted_owner(
            "https://github.com/MikesRuthless12/freally-capture/releases/download/v1.0.0/setup.exe"
        ));
        // Owner comparison is case-insensitive (GitHub logins are).
        assert!(trusted_owner(
            "https://github.com/mikesruthless12/freally-capture/releases/download/v1.0.0/setup.exe"
        ));
        assert!(!trusted_owner(
            "https://github.com/evil-fork/freally-capture/releases/download/v1.0.0/setup.exe"
        ));
        assert!(!trusted_owner(
            "https://evil.example/MikesRuthless12/freally-capture/releases/download/v1.0.0/x.exe"
        ));
        assert!(!trusted_owner(
            "http://github.com/MikesRuthless12/r/releases/download/v1/x.exe"
        ));
        assert!(!trusted_owner("not a url"));
        assert!(!trusted_owner("https://github.com/"));
        // A trusted owner is not enough — the URL must be the full
        // release-asset shape (the gate re-validates, it doesn't trust the
        // registry to have done so).
        assert!(!trusted_owner("https://github.com/MikesRuthless12/repo"));
        assert!(!trusted_owner(
            "https://github.com/MikesRuthless12/repo/archive/main.zip"
        ));
    }

    #[test]
    fn ramp_fraction_is_monotonic_and_capped() {
        let base = 0.25;
        let mut last = 0.0;
        for secs in [0u64, 1, 5, 10, 30, 120, 3600] {
            let f = ramp_fraction(base, Duration::from_secs(secs));
            assert!(f >= last, "ramp went backwards at {secs}s");
            assert!(f >= base && f <= 0.90);
            last = f;
        }
        // It approaches, but never claims, completion.
        assert!(ramp_fraction(base, Duration::from_secs(3600)) < 0.905);
    }

    #[test]
    fn safe_token_accepts_product_names_and_refuses_injection_shapes() {
        assert_eq!(
            safe_token("freally-capture", false).as_deref(),
            Ok("freally-capture")
        );
        assert_eq!(
            safe_token(" Freally Capture ", true).as_deref(),
            Ok("Freally Capture")
        );
        // Flags, paths, and separators are refused outright.
        assert!(safe_token("-rf", true).is_err());
        assert!(safe_token("a/b", true).is_err());
        assert!(safe_token("a\\b", true).is_err());
        assert!(safe_token("a;b", true).is_err());
        assert!(safe_token("", true).is_err());
        assert!(safe_token("Freally Capture", false).is_err()); // ids take no spaces
        assert!(safe_token(&"x".repeat(101), true).is_err());
    }

    #[test]
    fn group_elevation_is_only_for_linux_packages() {
        assert!(needs_group_elevation(InstallerKind::Deb));
        assert!(needs_group_elevation(InstallerKind::Rpm));
        assert!(!needs_group_elevation(InstallerKind::NsisExe));
        assert!(!needs_group_elevation(InstallerKind::Msi));
        assert!(!needs_group_elevation(InstallerKind::Dmg));
        assert!(!needs_group_elevation(InstallerKind::AppImage));
    }
}
