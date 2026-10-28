mod claude;
mod codex;
mod grok;
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
        "grok" | "grok-build" => Ok(SessionSource::Grok),
        _ => bail!("unknown source: {raw}"),
    }
}

pub fn list_sessions(source: SessionSource) -> Result<Vec<SessionSummary>> {
    match source {
        SessionSource::Codex => codex::list_sessions(),
        SessionSource::Claude => claude::list_sessions(),
        SessionSource::Grok => grok::list_sessions(),
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
        SessionSource::Grok => grok::read_session_detail(&path),
    }
}

pub fn source_root(source: SessionSource) -> Result<PathBuf> {
    match source {
        SessionSource::Codex => Ok(util::codex_root()?.join("sessions")),
        SessionSource::Claude => util::claude_root(),
        SessionSource::Grok => Ok(util::grok_root()?.join("sessions")),
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
    discover_session_files_at(source, &root)
}

pub fn is_session_file(source: SessionSource, path: &Path) -> bool {
    match source {
        SessionSource::Grok => grok::is_transcript(path),
        _ => path.extension().and_then(|value| value.to_str()) == Some("jsonl"),
    }
}

/// Map companion-metadata changes back to the one canonical session transcript.
pub fn session_file_for_event(source: SessionSource, root: &Path, path: &Path) -> Option<PathBuf> {
    if source == SessionSource::Grok {
        if path.strip_prefix(root).ok()?.components().count() != 3 {
            return None;
        }
        if path.file_name()?.to_str()? == "summary.json" {
            return grok::preferred_transcript(path.parent()?);
        }
        if matches!(
            path.file_name()?.to_str()?,
            "updates.jsonl" | "chat_history.jsonl"
        ) && matches!(path.try_exists(), Ok(false))
        {
            return grok::preferred_transcript(path.parent()?);
        }
    }
    is_session_file(source, path).then(|| path.to_path_buf())
}

fn discover_session_files_at(
    source: SessionSource,
    root: &Path,
) -> Result<Vec<DiscoveredSessionFile>> {
    if !root.exists() {
        return Ok(Vec::new());
    }

    let mut files = WalkDir::new(root)
        .max_depth(if source == SessionSource::Grok {
            3
        } else {
            usize::MAX
        })
        .follow_links(false)
        .into_iter()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().is_file())
        .filter(|entry| source != SessionSource::Grok || entry.depth() == 3)
        .filter(|entry| is_session_file(source, entry.path()))
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
        SessionSource::Grok => Ok(HashMap::new()),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grok_discovery_and_watching_select_one_transcript_and_map_summary_changes() {
        let dir = tempfile::tempdir().unwrap();
        let session = dir.path().join("project/session");
        fs::create_dir_all(session.join("compaction")).unwrap();
        let history = session.join("chat_history.jsonl");
        fs::write(&history, "{}\n").unwrap();
        for filename in [
            "events.jsonl",
            "rewind_points.jsonl",
            "summary.json",
            "compaction/updates.jsonl",
        ] {
            fs::write(session.join(filename), "{}\n").unwrap();
        }
        let found = discover_session_files_at(SessionSource::Grok, dir.path()).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].path, history);
        let updates = session.join("updates.jsonl");
        fs::write(&updates, "{}\n").unwrap();
        let found = discover_session_files_at(SessionSource::Grok, dir.path()).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].path, updates);
        assert_eq!(
            session_file_for_event(SessionSource::Grok, dir.path(), &history),
            None
        );
        assert_eq!(
            session_file_for_event(
                SessionSource::Grok,
                dir.path(),
                &session.join("summary.json")
            ),
            Some(updates)
        );
        assert_eq!(
            session_file_for_event(
                SessionSource::Grok,
                dir.path(),
                &session.join("compaction/updates.jsonl")
            ),
            None
        );
        fs::remove_file(session.join("updates.jsonl")).unwrap();
        assert_eq!(
            session_file_for_event(
                SessionSource::Grok,
                dir.path(),
                &session.join("updates.jsonl")
            ),
            Some(history)
        );
        assert_eq!(parse_source("grok-build").unwrap(), SessionSource::Grok);
        assert_eq!(
            serde_json::to_string(&SessionSource::Grok).unwrap(),
            "\"grok\""
        );
    }
}
