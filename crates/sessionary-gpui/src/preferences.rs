use std::{collections::HashMap, fs, path::Path};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sessionary_core::SessionSource;

use crate::indexer::app_data_dir;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ThemePreference {
    Light,
    Dark,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ViewMode {
    Recent,
    Grouped,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct ScrollAnchor {
    pub item_index: usize,
    pub offset_in_item: f32,
    #[serde(default)]
    pub is_following_tail: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Preferences {
    pub theme: ThemePreference,
    pub source: String,
    pub view_mode: ViewMode,
    pub hide_subagents: bool,
    pub scroll_anchors: HashMap<String, ScrollAnchor>,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            theme: ThemePreference::Light,
            source: "codex".to_string(),
            view_mode: ViewMode::Recent,
            hide_subagents: true,
            scroll_anchors: HashMap::new(),
        }
    }
}

impl Preferences {
    pub fn load() -> Self {
        let Ok(path) = preferences_path() else {
            return Self::default();
        };
        Self::load_from(&path)
    }

    fn load_from(path: &Path) -> Self {
        fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> Result<()> {
        let path = preferences_path()?;
        self.save_to(&path)
    }

    fn save_to(&self, path: &Path) -> Result<()> {
        let parent = path.parent().context("preferences path has no parent")?;
        fs::create_dir_all(parent)?;
        let temporary = path.with_extension("json.tmp");
        fs::write(&temporary, serde_json::to_vec_pretty(self)?)?;
        fs::rename(&temporary, path).with_context(|| {
            format!(
                "failed to replace Sessionary preferences {}",
                path.display()
            )
        })?;
        Ok(())
    }

    pub fn source(&self) -> SessionSource {
        match self.source.as_str() {
            "claude" => SessionSource::Claude,
            _ => SessionSource::Codex,
        }
    }

    pub fn set_source(&mut self, source: SessionSource) {
        self.source = match source {
            SessionSource::Claude => "claude",
            SessionSource::Codex => "codex",
        }
        .to_string();
    }

    pub fn scroll_anchor(&self, path: &Path) -> Option<ScrollAnchor> {
        self.scroll_anchors
            .get(path.to_string_lossy().as_ref())
            .copied()
    }

    pub fn set_scroll_anchor(&mut self, path: &Path, anchor: ScrollAnchor) {
        self.scroll_anchors
            .insert(path.to_string_lossy().into_owned(), anchor);
    }
}

fn preferences_path() -> Result<std::path::PathBuf> {
    Ok(app_data_dir()?.join("preferences.json"))
}

#[cfg(test)]
mod tests {
    use anyhow::Result;
    use tempfile::tempdir;

    use super::{Preferences, ScrollAnchor, ThemePreference, ViewMode};

    #[test]
    fn preferences_round_trip_native_state() -> Result<()> {
        let directory = tempdir()?;
        let path = directory.path().join("preferences.json");
        let session = directory.path().join("session.jsonl");
        let mut preferences = Preferences {
            theme: ThemePreference::Dark,
            view_mode: ViewMode::Grouped,
            hide_subagents: false,
            ..Preferences::default()
        };
        preferences.set_scroll_anchor(
            &session,
            ScrollAnchor {
                item_index: 42,
                offset_in_item: 17.5,
                is_following_tail: false,
            },
        );
        preferences.save_to(&path)?;

        let loaded = Preferences::load_from(&path);
        assert_eq!(loaded.theme, ThemePreference::Dark);
        assert_eq!(loaded.view_mode, ViewMode::Grouped);
        assert!(!loaded.hide_subagents);
        let anchor = loaded.scroll_anchor(&session).expect("anchor should exist");
        assert_eq!(anchor.item_index, 42);
        assert_eq!(anchor.offset_in_item, 17.5);
        assert!(!anchor.is_following_tail);
        Ok(())
    }
}
