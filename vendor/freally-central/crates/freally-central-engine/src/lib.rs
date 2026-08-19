//! The Freally Central engine: install detection, verified downloads, silent
//! install, and app launch. Freally Central's own shell and every host app
//! embedding the "Central inside" panel share this one implementation.
//!
//! A host wires it up in two places:
//!
//! ```ignore
//! freally_central_engine::attach(tauri::Builder::default())
//!     .invoke_handler(tauri::generate_handler![
//!         // ...the host's own commands...
//!         freally_central_engine::detect::central_detect_installed,
//!         freally_central_engine::download::central_start_download,
//!         freally_central_engine::download::central_cancel_download,
//!         freally_central_engine::install::central_install_apps,
//!         freally_central_engine::install::central_cancel_installs,
//!         freally_central_engine::install::central_launch_app,
//!         freally_central_engine::central_platform,
//!     ])
//! ```
//!
//! Command names carry a `central_` prefix so they can never collide with a
//! host app's own commands.

pub mod detect;
pub mod download;
pub mod install;
pub mod platform;

pub use platform::central_platform;

/// Register the engine's managed state on a Tauri builder. Call exactly once,
/// before `run`.
pub fn attach<R: tauri::Runtime>(builder: tauri::Builder<R>) -> tauri::Builder<R> {
    builder
        .manage(download::Downloads::default())
        .manage(download::VerifiedDownloads::default())
        .manage(install::Installs::default())
}
