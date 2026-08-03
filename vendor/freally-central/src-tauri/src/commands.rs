use serde::Serialize;

/// Build metadata for the About panel. Everything derivable from the build comes
/// from the build, so the panel can never claim a version the binary isn't.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BuildInfo {
    version: String,
    authors: String,
    project_started: String,
    first_stable_released: Option<String>,
    os: String,
    arch: String,
    homepage: String,
    repository: String,
    issues: String,
    copyright: String,
}

const REPOSITORY: &str = "https://github.com/MikesRuthless12/freally-central";
const PROJECT_STARTED: &str = "2026-07-14";
const FIRST_STABLE_RELEASED: Option<&str> = None;

#[tauri::command]
pub fn build_info() -> BuildInfo {
    BuildInfo {
        version: env!("CARGO_PKG_VERSION").to_string(),
        authors: "Mike Weaver (Havoc Software)".to_string(),
        project_started: PROJECT_STARTED.to_string(),
        first_stable_released: FIRST_STABLE_RELEASED.map(str::to_string),
        os: std::env::consts::OS.to_string(),
        arch: std::env::consts::ARCH.to_string(),
        homepage: "https://mikesruthless12.github.io/freally-central/".to_string(),
        repository: REPOSITORY.to_string(),
        issues: format!("{REPOSITORY}/issues"),
        copyright: "\u{00a9} 2026 Mike Weaver (Havoc Software) \u{2014} All Rights Reserved"
            .to_string(),
    }
}

/// The running build's changelog section, for What's New.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseNotes {
    version: String,
    notes: Option<String>,
}

// Embedded at build time so What's New is offline and always matches what shipped.
const CHANGELOG: &str = include_str!("../../CHANGELOG.md");

#[tauri::command]
pub fn release_notes() -> ReleaseNotes {
    ReleaseNotes {
        version: env!("CARGO_PKG_VERSION").to_string(),
        notes: latest_section(CHANGELOG),
    }
}

/// Returns the body of the first `## ` section of the changelog (Keep-a-Changelog style).
fn latest_section(changelog: &str) -> Option<String> {
    let mut collecting = false;
    let mut out = String::new();
    for line in changelog.lines() {
        if let Some(stripped) = line.strip_prefix("## ") {
            if collecting {
                break;
            }
            collecting = true;
            out.push_str(stripped);
            out.push('\n');
            continue;
        }
        if collecting {
            out.push_str(line);
            out.push('\n');
        }
    }
    let trimmed = out.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}
