use std::{
    collections::{HashMap, HashSet},
    path::Path,
};

use agentoria_core::index::CancellationToken;
use nucleo_matcher::{
    pattern::{AtomKind, CaseMatching, Normalization, Pattern},
    Config, Matcher, Utf32Str,
};

#[derive(Default)]
pub struct WorkdirFilter {
    pub query: String,
    pub matching: bool,
    candidates: HashSet<String>,
    scores: HashMap<String, u32>,
    revision: u64,
    dirty: bool,
    cancellation: CancellationToken,
}

pub struct MatchRequest {
    query: String,
    directories: Vec<String>,
    revision: u64,
    cancellation: CancellationToken,
}

pub struct MatchResult {
    revision: u64,
    scores: HashMap<String, u32>,
}

impl WorkdirFilter {
    pub fn set_query(&mut self, query: String) -> bool {
        if self.query == query {
            return false;
        }
        self.query = query;
        self.reset();
        true
    }

    /// Keep the query across source changes, but invalidate all old matches.
    pub fn reset(&mut self) {
        self.cancellation.cancel();
        self.revision = self.revision.wrapping_add(1);
        self.candidates.clear();
        self.scores.clear();
        self.matching = false;
        self.dirty = true;
    }

    pub fn prepare(&mut self, directories: HashSet<String>) -> Option<MatchRequest> {
        if self.query.trim().is_empty() {
            self.candidates = directories;
            self.matching = false;
            self.dirty = false;
            return None;
        }
        if !self.dirty && self.candidates == directories {
            return None;
        }
        self.cancellation.cancel();
        self.cancellation = CancellationToken::default();
        self.revision = self.revision.wrapping_add(1);
        self.candidates = directories;
        self.dirty = false;
        self.matching = true;
        Some(MatchRequest {
            query: self.query.clone(),
            directories: self.candidates.iter().cloned().collect(),
            revision: self.revision,
            cancellation: self.cancellation.clone(),
        })
    }

    pub fn apply(&mut self, result: MatchResult) -> bool {
        if self.revision != result.revision {
            return false;
        }
        self.scores = result.scores;
        self.matching = false;
        true
    }

    pub fn includes(&self, directory: Option<&str>) -> bool {
        self.query.trim().is_empty() || directory.is_some_and(|path| self.scores.contains_key(path))
    }

    pub fn score(&self, directory: &str) -> u32 {
        self.scores.get(directory).copied().unwrap_or(0)
    }
}

impl Drop for WorkdirFilter {
    fn drop(&mut self) {
        self.cancellation.cancel();
    }
}

impl MatchRequest {
    /// The caller runs this on GPUI's background executor. Reuse one matcher and
    /// UTF-32 scratch buffer, and score each unique directory only once.
    pub fn run(self, home: Option<&Path>) -> Option<MatchResult> {
        let query = expand_home(&self.query, home);
        let pattern = Pattern::parse(&query, CaseMatching::Smart, Normalization::Smart);
        // 0.3.1's case-sensitive ASCII prefix/postfix path compares the full
        // haystack instead of its selected slice. Its Unicode path is correct.
        let needs_unicode = pattern
            .atoms
            .iter()
            .any(|atom| matches!(atom.kind, AtomKind::Prefix | AtomKind::Postfix));
        let mut matcher = Matcher::new(Config::DEFAULT.match_paths());
        let mut buffer = Vec::new();
        let mut scores = HashMap::new();
        for directory in self.directories {
            if self.cancellation.is_cancelled() {
                return None;
            }
            let haystack = if needs_unicode && directory.is_ascii() {
                buffer.clear();
                buffer.extend(directory.chars());
                Utf32Str::Unicode(&buffer)
            } else {
                Utf32Str::new(&directory, &mut buffer)
            };
            if let Some(score) = pattern.score(haystack, &mut matcher) {
                scores.insert(directory, score);
            }
        }
        if self.cancellation.is_cancelled() {
            return None;
        }
        Some(MatchResult {
            revision: self.revision,
            scores,
        })
    }
}

fn expand_home(query: &str, home: Option<&Path>) -> String {
    let Some(home) = home.and_then(Path::to_str) else {
        return query.to_string();
    };
    // Expand ~/ at the start of an atom, after its optional match operator.
    query
        .split(' ')
        .map(|atom| {
            let start = atom.find('~');
            if let Some(start) = start {
                if atom[..start]
                    .chars()
                    .all(|ch| matches!(ch, '!' | '^' | '\''))
                    && atom[start..].starts_with("~/")
                {
                    return format!("{}{}{}", &atom[..start], home, &atom[start + 1..]);
                }
            }
            atom.to_string()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn search(query: &str, directories: &[&str]) -> WorkdirFilter {
        let mut filter = WorkdirFilter::default();
        filter.set_query(query.into());
        let request = filter
            .prepare(directories.iter().map(|path| path.to_string()).collect())
            .unwrap();
        assert!(filter.apply(request.run(Some(Path::new("/Users/test"))).unwrap()));
        filter
    }

    #[test]
    fn fuzzy_path_search_supports_terms_operators_unicode_and_home() {
        let paths = [
            "/Users/test/codes/agentoria",
            "/Users/test/codes/agent-origami",
            "/Users/test/文档/测试",
            "/elsewhere/agentoria",
        ];
        let filter = search("codes agt", &paths);
        assert!(filter.includes(Some(paths[0])));
        assert!(filter.includes(Some(paths[1])));
        assert!(!filter.includes(Some(paths[3])));
        assert!(search("agentoria$", &paths).includes(Some(paths[0])));
        assert!(search("'agentoria", &paths).includes(Some(paths[0])));
        assert!(!search("!codes agentoria", &paths).includes(Some(paths[0])));
        assert!(search("!codes agentoria", &paths).includes(Some(paths[3])));
        let expanded = expand_home("^~/codes agentoria", Some(Path::new("/Users/test")));
        assert_eq!(expanded, "^/Users/test/codes agentoria");
        assert!(search("^~/codes agentoria", &paths).includes(Some(paths[0])));
        assert!(search("^/Users", &paths).includes(Some(paths[0])));
        assert!(search("README$", &["/work/README"]).includes(Some("/work/README")));
        assert!(search("文 测", &paths).includes(Some(paths[2])));
        assert!(!search("CODES", &paths).includes(Some(paths[0])));
        assert!(!filter.includes(None));
    }

    #[test]
    fn changed_query_source_and_directory_set_discard_obsolete_results() {
        let mut filter = WorkdirFilter::default();
        filter.set_query("a".into());
        let first = filter
            .prepare(HashSet::from(["/a".into()]))
            .unwrap()
            .run(None)
            .unwrap();
        filter.set_query("b".into());
        assert!(!filter.apply(first));
        let second = filter.prepare(HashSet::from(["/b".into()])).unwrap();
        assert!(filter.prepare(HashSet::from(["/b".into()])).is_none());
        filter.reset();
        assert!(second.run(None).is_none());
        let current = filter
            .prepare(HashSet::from(["/b".into()]))
            .unwrap()
            .run(None)
            .unwrap();
        assert!(filter.apply(current));
        assert!(filter.includes(Some("/b")));
        filter.set_query(String::new());
        assert!(filter.includes(None));
        assert!(filter.includes(Some("/unmatched")));
    }

    #[test]
    fn matches_ten_thousand_directories_within_the_background_budget() {
        let directories = (0..10_000)
            .map(|index| format!("/Users/test/projects/{index}/agentoria"))
            .collect();
        let mut filter = WorkdirFilter::default();
        filter.set_query("agt".into());
        let request = filter.prepare(directories).unwrap();
        let started = std::time::Instant::now();
        let result = request.run(None).unwrap();
        let elapsed = started.elapsed();
        assert_eq!(result.scores.len(), 10_000);
        eprintln!("10,000 work directories: {elapsed:?}");
        if !cfg!(debug_assertions) {
            assert!(
                elapsed < std::time::Duration::from_millis(500),
                "{elapsed:?}"
            );
        }
    }
}
