pub mod claude;
pub mod codex;
mod model;
mod util;

use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use tauri::async_runtime;

pub use model::*;

#[tauri::command]
pub async fn list_sessions(source: String) -> Result<Vec<SessionSummary>, String> {
    let source = parse_source(&source).map_err(|error| error.to_string())?;

    async_runtime::spawn_blocking(move || match source {
        SessionSource::Codex => codex::list_sessions(),
        SessionSource::Claude => claude::list_sessions(),
    })
    .await
    .map_err(util::join_error_to_string)?
    .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn get_session_detail(source: String, path: String) -> Result<SessionDetail, String> {
    let source = parse_source(&source).map_err(|error| error.to_string())?;
    let path = validate_session_path(&source, &path).map_err(|error| error.to_string())?;

    async_runtime::spawn_blocking(move || match source {
        SessionSource::Codex => codex::read_session_detail(&path),
        SessionSource::Claude => claude::read_session_detail(&path),
    })
    .await
    .map_err(util::join_error_to_string)?
    .map_err(|error| error.to_string())
}

fn parse_source(raw: &str) -> Result<SessionSource> {
    match raw {
        "codex" => Ok(SessionSource::Codex),
        "claude" => Ok(SessionSource::Claude),
        _ => bail!("unknown source: {raw}"),
    }
}

fn validate_session_path(source: &SessionSource, raw_path: &str) -> Result<PathBuf> {
    let candidate = PathBuf::from(raw_path);
    let base = match source {
        SessionSource::Codex => util::codex_root()?.join("sessions"),
        SessionSource::Claude => util::claude_root()?,
    };

    let canonical_base = base
        .canonicalize()
        .with_context(|| format!("failed to canonicalize {}", base.display()))?;
    let canonical_candidate = candidate
        .canonicalize()
        .with_context(|| format!("failed to canonicalize {}", candidate.display()))?;

    if !canonical_candidate.starts_with(&canonical_base) {
        bail!("session path is outside of the expected source root");
    }

    Ok(canonical_candidate)
}
