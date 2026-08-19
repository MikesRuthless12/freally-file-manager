mod commands;

use freally_central_engine as engine;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    engine::attach(tauri::Builder::default())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .invoke_handler(tauri::generate_handler![
            commands::build_info,
            commands::release_notes,
            engine::detect::central_detect_installed,
            engine::download::central_start_download,
            engine::download::central_cancel_download,
            engine::install::central_install_apps,
            engine::install::central_cancel_installs,
            engine::install::central_launch_app,
            engine::central_platform
        ])
        .run(tauri::generate_context!())
        .expect("error while running Freally Central");
}
