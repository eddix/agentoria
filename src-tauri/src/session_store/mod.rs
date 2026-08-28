use sessionary_core::{SessionDetail, SessionSummary};
use tauri::async_runtime;

#[tauri::command]
pub async fn list_sessions(source: String) -> Result<Vec<SessionSummary>, String> {
    let source = sessionary_core::parse_source(&source).map_err(|error| error.to_string())?;

    async_runtime::spawn_blocking(move || sessionary_core::list_sessions(source))
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn get_session_detail(source: String, path: String) -> Result<SessionDetail, String> {
    let source = sessionary_core::parse_source(&source).map_err(|error| error.to_string())?;

    async_runtime::spawn_blocking(move || sessionary_core::get_session_detail(source, path))
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())
}
