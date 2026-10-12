use std::ops::Range;

use super::{load_conversation_line, CancellationToken, ConversationBody, IndexedLine};

/// A UTF-8 source range in one indexed conversation message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchMatch {
    pub conversation_index: usize,
    pub range: Range<usize>,
}

/// Only match locations are retained; message bodies are read one at a time.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct SearchResults {
    pub matches: Vec<SearchMatch>,
    pub total_matches: usize,
    pub oversized: usize,
    pub unreadable: usize,
}

/// Search literal message text using Unicode lowercase matching. Source offsets
/// remain valid when lowercase expands a character (for example, `İ`). Results
/// are capped, but the total count and skipped-message counts cover the session.
/// Cancellation returns `None`, so callers cannot publish an incomplete scan.
pub fn search_conversations(
    lines: &[IndexedLine],
    query: &str,
    max_source_bytes: u64,
    max_matches: usize,
    cancellation: &CancellationToken,
) -> Option<SearchResults> {
    let mut result = SearchResults::default();
    if query.is_empty() {
        return (!cancellation.is_cancelled()).then_some(result);
    }
    let query = lowercase(query);
    for (conversation_index, line) in lines.iter().enumerate() {
        if cancellation.is_cancelled() {
            return None;
        }
        match load_conversation_line(line, max_source_bytes) {
            Ok(Some(ConversationBody::Loaded(message))) => {
                let (ranges, total) = match_ranges(
                    &message.text,
                    &query,
                    max_matches.saturating_sub(result.matches.len()),
                );
                result.total_matches = result.total_matches.saturating_add(total);
                result
                    .matches
                    .extend(ranges.into_iter().map(|range| SearchMatch {
                        conversation_index,
                        range,
                    }));
            }
            Ok(Some(ConversationBody::Oversized { .. })) => result.oversized += 1,
            Ok(None) | Err(_) => result.unreadable += 1,
        }
    }
    (!cancellation.is_cancelled()).then_some(result)
}

fn lowercase(text: &str) -> String {
    text.chars().flat_map(char::to_lowercase).collect()
}

fn match_ranges(text: &str, folded_query: &str, limit: usize) -> (Vec<Range<usize>>, usize) {
    let ascii = text.is_ascii();
    let mut folded = if ascii {
        text.to_ascii_lowercase()
    } else {
        String::with_capacity(text.len())
    };
    let mut positions = Vec::new();
    if !ascii {
        for (offset, ch) in text.char_indices() {
            if limit > 0 {
                positions.push((folded.len(), offset, offset + ch.len_utf8()));
            }
            folded.extend(ch.to_lowercase());
        }
    }
    let mut ranges = Vec::new();
    let mut total = 0usize;
    for (start, found) in folded.match_indices(folded_query) {
        total = total.saturating_add(1);
        if ranges.len() >= limit {
            continue;
        }
        let end = start + found.len();
        let range = if ascii {
            start..end
        } else {
            let first = positions
                .partition_point(|(folded, _, _)| *folded <= start)
                .saturating_sub(1);
            let last = positions
                .partition_point(|(folded, _, _)| *folded < end)
                .saturating_sub(1);
            positions[first].1..positions[last].2
        };
        ranges.push(range);
    }
    (ranges, total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::ConversationKind;
    use crate::SessionSource;
    use std::{fs, path::Path};

    fn line(
        path: &Path,
        source: SessionSource,
        size: usize,
        kind: ConversationKind,
    ) -> IndexedLine {
        IndexedLine {
            file_id: 1,
            source,
            path: path.to_path_buf(),
            ordinal: 0,
            line_number: 1,
            byte_offset: 0,
            byte_len: size as u64,
            event_type: String::new(),
            timestamp: None,
            conversation_kind: Some(kind),
            process_count: 0,
        }
    }

    #[test]
    fn unicode_lowercase_preserves_utf8_source_offsets() {
        let text = "中文 İSTANBUL Rust rUsT";
        let (ranges, total) = match_ranges(text, &lowercase("rust"), 10);
        assert_eq!(total, 2);
        assert_eq!(
            ranges.iter().map(|r| &text[r.clone()]).collect::<Vec<_>>(),
            ["Rust", "rUsT"]
        );
        let (ranges, _) = match_ranges(text, &lowercase("i"), 10);
        assert_eq!(&text[ranges[0].clone()], "İ");
        let (ranges, _) = match_ranges(text, &lowercase("中文"), 10);
        assert_eq!(&text[ranges[0].clone()], "中文");
    }

    #[test]
    fn search_reads_codex_and_claude_text_and_keeps_counts_after_cap() {
        let dir = tempfile::tempdir().unwrap();
        let codex = dir.path().join("codex.jsonl");
        let claude = dir.path().join("claude.jsonl");
        let a = r#"{"type":"event_msg","payload":{"type":"user_message","message":"Rust\nRUST"}}"#;
        let b = r#"{"type":"assistant","message":{"content":[{"type":"text","text":"rust"}]}}"#;
        fs::write(&codex, a).unwrap();
        fs::write(&claude, b).unwrap();
        let lines = [
            line(
                &codex,
                SessionSource::Codex,
                a.len(),
                ConversationKind::User,
            ),
            line(
                &claude,
                SessionSource::Claude,
                b.len(),
                ConversationKind::Assistant,
            ),
        ];
        let result =
            search_conversations(&lines, "rUsT", 1024, 2, &CancellationToken::default()).unwrap();
        assert_eq!(result.total_matches, 3);
        assert_eq!(
            result.matches,
            [
                SearchMatch {
                    conversation_index: 0,
                    range: 0..4
                },
                SearchMatch {
                    conversation_index: 0,
                    range: 5..9
                }
            ]
        );
        assert_eq!(result.oversized + result.unreadable, 0);
    }

    #[test]
    fn skipped_records_and_cancellation_are_explicit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing.jsonl");
        let lines = [
            line(&path, SessionSource::Codex, 1025, ConversationKind::User),
            line(
                &path,
                SessionSource::Codex,
                100,
                ConversationKind::Assistant,
            ),
        ];
        let result =
            search_conversations(&lines, "x", 1024, 10, &CancellationToken::default()).unwrap();
        assert_eq!((result.oversized, result.unreadable), (1, 1));
        let token = CancellationToken::default();
        token.cancel();
        assert!(search_conversations(&lines, "x", 1024, 10, &token).is_none());
        let result =
            search_conversations(&lines, "", 1024, 10, &CancellationToken::default()).unwrap();
        assert_eq!(result, SearchResults::default());
    }
}
