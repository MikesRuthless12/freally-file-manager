use serde::Serialize;

/// What the panel UI needs to pick the right installer asset.
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PlatformInfo {
    pub os: &'static str,
    pub arch: &'static str,
}

/// Host-agnostic platform probe for the embedded panel UI.
#[tauri::command]
pub fn central_platform() -> PlatformInfo {
    PlatformInfo {
        os: std::env::consts::OS,
        arch: std::env::consts::ARCH,
    }
}
