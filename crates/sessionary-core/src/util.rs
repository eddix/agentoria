use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::Value;

pub fn home_dir() -> Result<PathBuf> {
    dirs::home_dir().context("failed to resolve home directory")
}

pub fn codex_root() -> Result<PathBuf> {
    Ok(home_dir()?.join(".codex"))
}

pub fn claude_root() -> Result<PathBuf> {
    Ok(home_dir()?.join(".claude").join("projects"))
}

pub fn metadata_timestamp(path: &Path) -> Result<String> {
    let metadata = fs::metadata(path)
        .with_context(|| format!("failed to read metadata for {}", path.display()))?;
    let modified = metadata
        .modified()
        .context("failed to read modified time")?;
    Ok(DateTime::<Utc>::from(modified).to_rfc3339())
}

pub fn update_latest_timestamp(latest: &mut Option<String>, candidate: Option<&str>) {
    let Some(candidate) = candidate else {
        return;
    };
    let Ok(candidate_at) = DateTime::parse_from_rfc3339(candidate) else {
        return;
    };

    let should_replace = match latest
        .as_deref()
        .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
    {
        Some(latest_at) => candidate_at > latest_at,
        None => true,
    };

    if should_replace {
        *latest = Some(
            candidate_at
                .with_timezone(&Utc)
                .to_rfc3339_opts(SecondsFormat::Millis, true),
        );
    }
}

pub fn resolve_session_updated_at(
    last_event: Option<String>,
    file_modified: Option<String>,
    indexed: Option<&str>,
) -> String {
    last_event
        .or(file_modified)
        .or_else(|| indexed.map(ToOwned::to_owned))
        .map(|value| {
            DateTime::parse_from_rfc3339(&value)
                .map(|timestamp| {
                    timestamp
                        .with_timezone(&Utc)
                        .to_rfc3339_opts(SecondsFormat::Millis, true)
                })
                .unwrap_or(value)
        })
        .unwrap_or_default()
}

pub fn clean_title(raw: Option<&str>) -> Option<String> {
    raw.and_then(|text| {
        let cleaned = text
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .unwrap_or("")
            .chars()
            .take(120)
            .collect::<String>();
        if cleaned.is_empty() {
            None
        } else {
            Some(cleaned)
        }
    })
}

pub fn clean_preview(raw: &str) -> String {
    raw.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(220)
        .collect()
}

/// Whether a user prompt carries real signal — used to pick the "last prompt"
/// for titles/previews, skipping slash-commands, command markup, and throwaway
/// acks like "继续" / "ok".
pub fn is_meaningful_prompt(raw: &str) -> bool {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return false;
    }
    if trimmed.starts_with('/') {
        return false;
    }
    // Claude wraps invoked slash-commands as <command-name>…</command-name>,
    // and echoes local command output as <local-command-stdout>…
    if trimmed.starts_with("<command-") || trimmed.starts_with("<local-command") {
        return false;
    }
    trimmed.chars().count() > 3
}

pub fn file_stem_id(path: &Path) -> String {
    let Some(stem) = path.file_stem().and_then(|value| value.to_str()) else {
        return "session".to_string();
    };

    if let Some(candidate) = stem.get(stem.len().saturating_sub(36)..) {
        let bytes = candidate.as_bytes();
        let looks_like_uuid = bytes.len() == 36
            && matches!(bytes.get(8), Some(b'-'))
            && matches!(bytes.get(13), Some(b'-'))
            && matches!(bytes.get(18), Some(b'-'))
            && matches!(bytes.get(23), Some(b'-'))
            && bytes.iter().enumerate().all(|(index, byte)| match index {
                8 | 13 | 18 | 23 => *byte == b'-',
                _ => byte.is_ascii_hexdigit(),
            });

        if looks_like_uuid {
            return candidate.to_string();
        }
    }

    stem.to_string()
}

pub fn pretty_json(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_default()
}

pub fn truncate_chars(input: &str, limit: usize) -> (String, bool) {
    let mut out = String::new();
    for (count, ch) in input.chars().enumerate() {
        if count >= limit {
            return (out, true);
        }
        out.push(ch);
    }
    (out, false)
}

#[cfg(test)]
mod tests {
    use super::{resolve_session_updated_at, update_latest_timestamp};

    #[test]
    fn update_latest_timestamp_keeps_the_newest_valid_event() {
        let mut latest = None;

        update_latest_timestamp(&mut latest, Some("2026-07-29T09:59:06.895Z"));
        update_latest_timestamp(&mut latest, Some("invalid"));
        update_latest_timestamp(&mut latest, Some("2026-07-28T10:00:00Z"));

        assert_eq!(latest.as_deref(), Some("2026-07-29T09:59:06.895Z"));
    }

    #[test]
    fn resolve_session_updated_at_prefers_event_then_mtime_then_index() {
        let updated_at = resolve_session_updated_at(
            Some("2026-07-29T09:59:06.895Z".to_string()),
            Some("2026-07-30T10:00:00Z".to_string()),
            Some("2026-07-31T10:00:00Z"),
        );

        assert_eq!(updated_at, "2026-07-29T09:59:06.895Z");
    }

    #[test]
    fn resolve_session_updated_at_falls_back_to_index_without_file_times() {
        let updated_at =
            resolve_session_updated_at(None, None, Some("2026-07-24T05:46:05.538863Z"));

        assert_eq!(updated_at, "2026-07-24T05:46:05.538Z");
    }
}
