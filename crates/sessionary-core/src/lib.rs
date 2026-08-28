mod claude;
mod codex;
pub mod index;
mod model;
mod util;

use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    time::SystemTime,
};

use anyhow::{bail, Context, Result};
use walkdir::WalkDir;

pub use model::*;

pub fn parse_source(raw: &str) -> Result<SessionSource> {
    match raw {
        "codex" => Ok(SessionSource::Codex),
        "claude" => Ok(SessionSource::Claude),
        _ => bail!("unknown source: {raw}"),
    }
}

pub fn list_sessions(source: SessionSource) -> Result<Vec<SessionSummary>> {
    match source {
        SessionSource::Codex => codex::list_sessions(),
        SessionSource::Claude => claude::list_sessions(),
    }
}

pub fn get_session_detail(
    source: SessionSource,
    raw_path: impl AsRef<Path>,
) -> Result<SessionDetail> {
    let path = validate_session_path(source, raw_path.as_ref())?;
    match source {
        SessionSource::Codex => codex::read_session_detail(&path),
        SessionSource::Claude => claude::read_session_detail(&path),
    }
}

pub fn source_root(source: SessionSource) -> Result<PathBuf> {
    match source {
        SessionSource::Codex => Ok(util::codex_root()?.join("sessions")),
        SessionSource::Claude => util::claude_root(),
    }
}

#[derive(Debug, Clone)]
pub struct DiscoveredSessionFile {
    pub path: PathBuf,
    pub size: u64,
    pub modified_at: Option<SystemTime>,
}

pub fn discover_session_files(source: SessionSource) -> Result<Vec<DiscoveredSessionFile>> {
    let root = source_root(source)?;
    if !root.exists() {
        return Ok(Vec::new());
    }

    let mut files = WalkDir::new(&root)
        .follow_links(false)
        .into_iter()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().is_file())
        .filter(|entry| entry.path().extension().and_then(|value| value.to_str()) == Some("jsonl"))
        .filter_map(|entry| {
            let metadata = fs::metadata(entry.path()).ok()?;
            Some(DiscoveredSessionFile {
                path: entry.into_path(),
                size: metadata.len(),
                modified_at: metadata.modified().ok(),
            })
        })
        .collect::<Vec<_>>();
    files.sort_by(|left, right| {
        right
            .modified_at
            .cmp(&left.modified_at)
            .then_with(|| left.path.cmp(&right.path))
    });
    Ok(files)
}

pub fn load_session_titles(source: SessionSource) -> Result<HashMap<PathBuf, String>> {
    match source {
        SessionSource::Claude => claude::session_titles(),
        SessionSource::Codex => codex::session_titles(),
    }
}

fn validate_session_path(source: SessionSource, candidate: &Path) -> Result<PathBuf> {
    let base = source_root(source)?;
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
