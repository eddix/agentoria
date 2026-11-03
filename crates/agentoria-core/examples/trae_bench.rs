//! Read-only checks of this user's Trae CN catalog and largest local session.
use agentoria_core::{
    index::{search::search_conversations, CancellationToken, ConversationBody},
    trae_cn,
};
use anyhow::{Context as _, Result};
use std::time::Instant;

fn main() -> Result<()> {
    let cancellation = CancellationToken::default();
    let started = Instant::now();
    let files = trae_cn::list_indexed_sessions(&cancellation)?;
    println!(
        "Trae CN sessions={} catalog_ms={:.2}",
        files.len(),
        started.elapsed().as_secs_f64() * 1000.
    );
    if std::env::args().any(|arg| arg == "--all") {
        let mut unreadable = 0;
        let mut oversized = 0;
        let mut mismatches = 0;
        let mut messages = 0;
        for file in files {
            let (_, lines, _) = trae_cn::load_snapshot(file, &cancellation)?;
            for line in lines {
                messages += 1;
                match trae_cn::load_conversation(&line, 512 * 1024) {
                    Ok(Some(ConversationBody::Loaded(body))) => {
                        mismatches += usize::from(body.text.len() as u64 != line.byte_len)
                    }
                    Ok(Some(ConversationBody::Oversized { .. })) => oversized += 1,
                    Err(error) => {
                        unreadable += 1;
                        println!(
                            "unreadable kind={:?} bytes={} reason={error:#}",
                            line.conversation_kind, line.byte_len
                        );
                    }
                    _ => unreadable += 1,
                }
            }
        }
        println!("all_messages={messages} unreadable={unreadable} oversized={oversized} length_mismatches={mismatches}");
        anyhow::ensure!(
            unreadable == 0 && mismatches == 0,
            "Native message verification failed"
        );
        return Ok(());
    }
    let Some(file) = files.into_iter().max_by_key(|file| file.process_count) else {
        return Ok(());
    };
    let started = Instant::now();
    let (file, lines, process) = trae_cn::load_snapshot(file, &cancellation)?;
    println!(
        "largest_session user={} assistant={} process_records={} metadata_ms={:.2}",
        file.user_count,
        file.assistant_count,
        process.len(),
        started.elapsed().as_secs_f64() * 1000.
    );
    let started = Instant::now();
    let mut bytes = 0;
    for line in lines.iter().take(16) {
        if let Some(ConversationBody::Loaded(body)) = trae_cn::load_conversation(line, 512 * 1024)?
        {
            bytes += body.text.len();
        }
    }
    println!(
        "first_16_body_bytes={bytes} load_ms={:.2}",
        started.elapsed().as_secs_f64() * 1000.
    );
    let started = Instant::now();
    let mut process_events = 0;
    for line in process.iter().take(16) {
        if let agentoria_core::index::ProcessBody::Loaded(events) =
            trae_cn::load_process(line, 512 * 1024)?
        {
            process_events += events.len();
        }
    }
    if let Some(line) = process.first() {
        assert!(trae_cn::read_chunk(line, 0, 64 * 1024)?.len() <= 64 * 1024);
    }
    println!(
        "first_16_process_events={process_events} load_ms={:.2}",
        started.elapsed().as_secs_f64() * 1000.
    );
    let started = Instant::now();
    let result = search_conversations(&lines, "test", 512 * 1024, 10_000, &cancellation)
        .context("Benchmark cancelled")?;
    println!(
        "search_matches={} unreadable={} oversized={} search_ms={:.2}",
        result.total_matches,
        result.unreadable,
        result.oversized,
        started.elapsed().as_secs_f64() * 1000.
    );
    Ok(())
}
