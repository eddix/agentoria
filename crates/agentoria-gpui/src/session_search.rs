use std::ops::Range;

use agentoria_core::index::{search::SearchResults, CancellationToken};
use gpui::{Entity, FocusHandle, Hsla, Subscription, Task};
use gpui_kit::{
    base::text::{RenderedText, TextViewState},
    component::input::InputState,
};

pub const MAX_SEARCH_MATCHES: usize = 10_000;

#[derive(Default)]
pub struct SessionSearch {
    pub open: bool,
    pub previous_focus: Option<FocusHandle>,
    pub input: Option<Entity<InputState>>,
    pub subscription: Option<Subscription>,
    pub query: String,
    pub revision: u64,
    pub cancellation: CancellationToken,
    pub task: Option<Task<()>>,
    pub searching: bool,
    pub results: SearchResults,
    pub active: Option<usize>,
    pub highlight: Option<MatchHighlight>,
}

pub struct MatchHighlight {
    pub conversation_index: usize,
    pub view: Entity<TextViewState>,
    pub _subscription: Subscription,
    pub stamp: Option<(RenderedText, Range<usize>, Hsla)>,
}

impl SessionSearch {
    pub fn reset(&mut self, query: String) {
        self.cancellation.cancel();
        self.cancellation = CancellationToken::default();
        self.task = None;
        self.query = query;
        self.revision = self.revision.wrapping_add(1);
        self.searching = !self.query.is_empty();
        self.results = SearchResults::default();
        self.active = None;
        self.highlight = None;
    }

    pub fn status(&self) -> String {
        if self.searching {
            return "Searching…".to_string();
        }
        if self.query.is_empty() {
            return "USER / ASSISTANT · Ignore case".to_string();
        }
        let shown = self.results.matches.len();
        let mut status = if shown == 0 {
            "No matches".to_string()
        } else {
            format!("{} / {}", self.active.map_or(0, |index| index + 1), shown)
        };
        if self.results.total_matches > shown {
            status.push_str(&format!(
                " · {} total; first {shown} shown",
                self.results.total_matches
            ));
        }
        if self.results.oversized > 0 {
            status.push_str(&format!(
                " · {} large messages not searched",
                self.results.oversized
            ));
        }
        if self.results.unreadable > 0 {
            status.push_str(&format!(
                " · {} messages unreadable",
                self.results.unreadable
            ));
        }
        status
    }
}

impl Drop for SessionSearch {
    fn drop(&mut self) {
        self.cancellation.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_query_and_drop_cancel_the_previous_scan() {
        let mut search = SessionSearch::default();
        search.reset("old".to_string());
        let old = search.cancellation.clone();
        search.reset("new".to_string());
        assert!(old.is_cancelled());
        let current = search.cancellation.clone();
        assert!(!current.is_cancelled());
        drop(search);
        assert!(current.is_cancelled());
    }

    #[test]
    fn partial_coverage_and_result_limit_are_visible_in_status() {
        let mut search = SessionSearch::default();
        search.reset("x".to_string());
        search.searching = false;
        search.results.oversized = 2;
        search.results.unreadable = 1;
        assert_eq!(
            search.status(),
            "No matches · 2 large messages not searched · 1 messages unreadable"
        );
        search
            .results
            .matches
            .push(agentoria_core::index::search::SearchMatch {
                conversation_index: 0,
                range: 0..1,
            });
        search.results.total_matches = 10;
        search.active = Some(0);
        assert!(search.status().contains("10 total; first 1 shown"));
    }
}
