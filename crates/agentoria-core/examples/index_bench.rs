use std::{env, path::PathBuf, time::Instant};

use agentoria_core::{
    index::{load_conversation_line, CancellationToken, IndexDatabase},
    SessionSource,
};
use anyhow::{bail, Context, Result};

fn main() -> Result<()> {
    let mut arguments = env::args().skip(1);
    let source = arguments
        .next()
        .context("usage: index_bench <claude|codex> <session.jsonl>")?;
    let source = agentoria_core::parse_source(&source)?;
    let session = PathBuf::from(
        arguments
            .next()
            .context("usage: index_bench <claude|codex> <session.jsonl>")?,
    );
    if arguments.next().is_some() {
        bail!("usage: index_bench <claude|codex> <session.jsonl>");
    }
    run(source, session)
}

fn run(source: SessionSource, session: PathBuf) -> Result<()> {
    let directory = tempfile::tempdir()?;
    let database_path = directory.path().join("benchmark.sqlite");
    let mut database = IndexDatabase::open(&database_path)?;

    let started = Instant::now();
    let outcome = database.scan_file(source, &session, &CancellationToken::default(), |_| {})?;
    let index_ms = started.elapsed().as_secs_f64() * 1000.;

    let query_started = Instant::now();
    let conversations = database.conversation_lines(outcome.file.id)?;
    let process = database.process_lines(outcome.file.id)?;
    let query_ms = query_started.elapsed().as_secs_f64() * 1000.;

    let load_started = Instant::now();
    let mut loaded = 0usize;
    for line in conversations.iter().rev().take(20) {
        if load_conversation_line(line, 512 * 1024)?.is_some() {
            loaded += 1;
        }
    }
    let load_ms = load_started.elapsed().as_secs_f64() * 1000.;
    let database_bytes = sqlite_bytes(&database_path);

    println!(
        "{{\"source\":\"{}\",\"file_bytes\":{},\"complete_lines\":{},\"conversation_rows\":{},\"process_lines\":{},\"errors\":{},\"index_ms\":{:.2},\"query_ms\":{:.2},\"last_20_loaded\":{},\"load_ms\":{:.2},\"database_bytes\":{}}}",
        match source {
            SessionSource::Claude => "claude",
            SessionSource::Codex => "codex",
        },
        outcome.file.file_size,
        outcome.file.line_count,
        conversations.len(),
        process.len(),
        outcome.file.error_count,
        index_ms,
        query_ms,
        loaded,
        load_ms,
        database_bytes,
    );
    Ok(())
}

fn sqlite_bytes(path: &std::path::Path) -> u64 {
    [
        path.to_path_buf(),
        PathBuf::from(format!("{}-wal", path.to_string_lossy())),
        PathBuf::from(format!("{}-shm", path.to_string_lossy())),
    ]
    .into_iter()
    .filter_map(|path| std::fs::metadata(path).ok())
    .map(|metadata| metadata.len())
    .sum()
}
