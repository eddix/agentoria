use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    sync::mpsc,
    time::Duration,
};

use agentoria_core::{
    discover_session_files,
    index::{CancellationToken, IndexDatabase, IndexedFile, ScanProgress},
    load_session_titles, source_root, SessionSource,
};
use anyhow::{Context, Result};
use async_channel::Sender;
use notify::{Event, EventKind, RecursiveMode, Watcher as _};
use rusqlite::{Connection, DatabaseName, OpenFlags};

#[derive(Debug)]
pub enum SourceLoadEvent {
    Cached(Vec<IndexedFile>),
    Discovered {
        total: usize,
    },
    Progress {
        path: PathBuf,
        file_index: usize,
        total_files: usize,
        scan: ScanProgress,
    },
    Indexed(Box<IndexedFile>),
    FileError {
        path: PathBuf,
        message: String,
    },
    Finished,
    Failed(String),
}

pub fn app_data_dir() -> Result<PathBuf> {
    dirs::data_dir()
        .context("failed to resolve Application Support directory")
        .map(|root| {
            root.join(if crate::IS_PREVIEW {
                "Agentoria GPUI Preview"
            } else {
                "Agentoria"
            })
        })
}

pub fn migrate_legacy_data() -> Result<()> {
    let root = dirs::data_dir().context("failed to resolve Application Support directory")?;
    migrate_legacy_data_at(&root, crate::IS_PREVIEW)
}

fn migrate_legacy_data_at(root: &Path, preview: bool) -> Result<()> {
    let destination = root.join(if preview {
        "Agentoria GPUI Preview"
    } else {
        "Agentoria"
    });
    let legacy_names: &[&str] = if preview {
        &["Sessionary GPUI Preview"]
    } else {
        &["Sessionary", "Sessionary GPUI Preview"]
    };
    let sources = legacy_names
        .iter()
        .map(|name| root.join(name))
        .filter(|path| path.is_dir())
        .collect::<Vec<_>>();
    if sources.is_empty() {
        return Ok(());
    }
    fs::create_dir_all(&destination)?;

    let preferences = destination.join("preferences.json");
    if !preferences.exists() {
        if let Some(source) = sources
            .iter()
            .map(|path| path.join("preferences.json"))
            .find(|path| path.is_file())
        {
            let temporary = destination.join("preferences.json.migrating");
            fs::copy(source, &temporary)?;
            fs::rename(temporary, preferences)?;
        }
    }

    let index = destination.join("index.sqlite");
    if !index.exists() {
        if let Some(source) = sources
            .iter()
            .map(|path| path.join("index.sqlite"))
            .find(|path| path.is_file())
        {
            let temporary = destination.join("index.sqlite.migrating");
            if temporary.exists() {
                fs::remove_file(&temporary)?;
            }
            let connection = Connection::open_with_flags(source, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
            connection.backup(DatabaseName::Main, &temporary, None)?;
            fs::rename(temporary, index)?;
        }
    }
    Ok(())
}

pub fn index_path() -> Result<PathBuf> {
    Ok(app_data_dir()?.join("index.sqlite"))
}

pub fn run_source_load(
    source: SessionSource,
    index_path: &Path,
    cancellation: &CancellationToken,
    sender: &Sender<SourceLoadEvent>,
) -> Result<()> {
    let mut database = IndexDatabase::open(index_path)?;
    sender
        .send_blocking(SourceLoadEvent::Cached(database.list_files(source)?))
        .ok();

    let files = discover_session_files(source)?;
    let titles = load_session_titles(source).unwrap_or_default();
    sender
        .send_blocking(SourceLoadEvent::Discovered { total: files.len() })
        .ok();
    let total_files = files.len();

    for (file_index, file) in files.into_iter().enumerate() {
        if cancellation.is_cancelled() {
            return Ok(());
        }
        let path = file.path;
        let result = database.scan_file(source, &path, cancellation, |scan| {
            sender
                .send_blocking(SourceLoadEvent::Progress {
                    path: path.clone(),
                    file_index,
                    total_files,
                    scan,
                })
                .ok();
        });
        match result {
            Ok(outcome) => {
                let canonical = path.canonicalize().unwrap_or_else(|_| path.clone());
                if let Some(title) = titles.get(&canonical) {
                    database.set_title(outcome.file.id, Some(title))?;
                }
                let indexed = database
                    .indexed_file(source, &canonical)?
                    .unwrap_or(outcome.file);
                sender
                    .send_blocking(SourceLoadEvent::Indexed(Box::new(indexed)))
                    .ok();
            }
            Err(_error) if cancellation.is_cancelled() => return Ok(()),
            Err(error) => {
                sender
                    .send_blocking(SourceLoadEvent::FileError {
                        path,
                        message: format!("{error:#}"),
                    })
                    .ok();
            }
        }
    }

    sender.send_blocking(SourceLoadEvent::Finished).ok();
    watch_source(source, &mut database, &titles, cancellation, sender)
}

fn watch_source(
    source: SessionSource,
    database: &mut IndexDatabase,
    titles: &std::collections::HashMap<PathBuf, String>,
    cancellation: &CancellationToken,
    sender: &Sender<SourceLoadEvent>,
) -> Result<()> {
    let root = source_root(source)?;
    if !root.exists() {
        return Ok(());
    }
    let (watch_sender, watch_receiver) = mpsc::channel();
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<Event>| {
        watch_sender.send(event).ok();
    })?;
    watcher.watch(&root, RecursiveMode::Recursive)?;

    while !cancellation.is_cancelled() {
        let first = match watch_receiver.recv_timeout(Duration::from_millis(400)) {
            Ok(event) => event,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };
        let mut changed = HashSet::new();
        collect_changed_paths(first, &mut changed);
        std::thread::sleep(Duration::from_millis(150));
        while let Ok(event) = watch_receiver.try_recv() {
            collect_changed_paths(event, &mut changed);
        }

        let total_files = database.list_files(source)?.len().max(1);
        for path in changed {
            if cancellation.is_cancelled() {
                return Ok(());
            }
            if !path.exists() {
                continue;
            }
            let result = database.scan_file(source, &path, cancellation, |scan| {
                sender
                    .send_blocking(SourceLoadEvent::Progress {
                        path: path.clone(),
                        file_index: total_files.saturating_sub(1),
                        total_files,
                        scan,
                    })
                    .ok();
            });
            match result {
                Ok(outcome) => {
                    let canonical = path.canonicalize().unwrap_or_else(|_| path.clone());
                    if let Some(title) = titles.get(&canonical) {
                        database.set_title(outcome.file.id, Some(title))?;
                    }
                    let indexed = database
                        .indexed_file(source, &canonical)?
                        .unwrap_or(outcome.file);
                    sender
                        .send_blocking(SourceLoadEvent::Indexed(Box::new(indexed)))
                        .ok();
                }
                Err(_error) if cancellation.is_cancelled() => return Ok(()),
                Err(error) => {
                    sender
                        .send_blocking(SourceLoadEvent::FileError {
                            path,
                            message: format!("{error:#}"),
                        })
                        .ok();
                }
            }
        }
        sender.send_blocking(SourceLoadEvent::Finished).ok();
    }
    Ok(())
}

fn collect_changed_paths(event: notify::Result<Event>, paths: &mut HashSet<PathBuf>) {
    let Ok(event) = event else {
        return;
    };
    if !matches!(
        event.kind,
        EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
    ) {
        return;
    }
    paths.extend(
        event
            .paths
            .into_iter()
            .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("jsonl")),
    );
}

#[cfg(test)]
mod tests {
    use std::fs;

    use anyhow::Result;
    use rusqlite::Connection;
    use tempfile::tempdir;

    use super::migrate_legacy_data_at;

    #[test]
    fn migrates_legacy_index_and_preferences_without_overwriting_new_data() -> Result<()> {
        let root = tempdir()?;
        let legacy = root.path().join("Sessionary");
        fs::create_dir_all(&legacy)?;
        fs::write(
            legacy.join("preferences.json"),
            br#"{"theme":"Flexoki Dark"}"#,
        )?;
        let database = Connection::open(legacy.join("index.sqlite"))?;
        database.execute_batch(
            "CREATE TABLE marker (value TEXT); INSERT INTO marker VALUES ('kept');",
        )?;
        drop(database);

        migrate_legacy_data_at(root.path(), false)?;
        let destination = root.path().join("Agentoria");
        assert_eq!(
            fs::read(destination.join("preferences.json"))?,
            br#"{"theme":"Flexoki Dark"}"#
        );
        let database = Connection::open(destination.join("index.sqlite"))?;
        let value: String = database.query_row("SELECT value FROM marker", [], |row| row.get(0))?;
        assert_eq!(value, "kept");
        drop(database);

        fs::write(legacy.join("preferences.json"), br#"{"theme":"Changed"}"#)?;
        migrate_legacy_data_at(root.path(), false)?;
        assert_eq!(
            fs::read(destination.join("preferences.json"))?,
            br#"{"theme":"Flexoki Dark"}"#
        );
        assert!(legacy.join("index.sqlite").exists());
        Ok(())
    }

    #[test]
    fn preview_migrates_only_from_legacy_preview() -> Result<()> {
        let root = tempdir()?;
        let legacy = root.path().join("Sessionary GPUI Preview");
        fs::create_dir_all(&legacy)?;
        fs::write(legacy.join("preferences.json"), b"preview")?;

        migrate_legacy_data_at(root.path(), true)?;
        assert_eq!(
            fs::read(root.path().join("Agentoria GPUI Preview/preferences.json"))?,
            b"preview"
        );
        assert!(!root.path().join("Agentoria/index.sqlite").exists());
        Ok(())
    }
}
