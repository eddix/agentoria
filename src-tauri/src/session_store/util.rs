use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
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

pub fn join_error_to_string(error: impl std::fmt::Display) -> String {
    format!("background task failed: {error}")
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
    let mut count = 0;
    for ch in input.chars() {
        if count >= limit {
            return (out, true);
        }
        out.push(ch);
        count += 1;
    }
    (out, false)
}

