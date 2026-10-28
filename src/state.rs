use crate::models::{PrGroup, PullRequest, ReviewReport, group_by_repo};
use crate::review::{ReviewEvent, Stage};
use ratatui::layout::Rect;
use ratatui::widgets::ListState;
use std::collections::HashMap;
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

const TOAST_LIFETIME: Duration = Duration::from_secs(8);

#[derive(Clone, Debug)]
pub enum ReviewStatus {
    Running(Stage),
    Done(ReviewReport),
    Failed(String),
}

#[derive(Clone, Debug)]
pub enum Row {
    Header { repo: String, count: usize },
    Pr(usize),
}

pub struct Toast {
    pub message: String,
    expires: Instant,
}

pub struct State {
    pub groups: Vec<PrGroup>,
    pub prs: Vec<PullRequest>,
    pub rows: Vec<Row>,
    pub selected_row: usize,
    pub list_state: ListState,
    pub reviews: HashMap<String, ReviewStatus>,
    pub active: Option<String>,
    pub detail_scroll: u16,
    pub toast: Option<Toast>,
    /// Repository filter, empty means every repo is shown.
    pub filter: String,
    pub filter_input: bool,
    pub list_area: Rect,
    pub detail_area: Rect,
}

impl State {
    pub fn new(prs: Vec<PullRequest>) -> Self {
        let mut state = Self {
            groups: Vec::new(),
            prs: Vec::new(),
            rows: Vec::new(),
            selected_row: 0,
            list_state: ListState::default(),
            reviews: HashMap::new(),
            active: None,
            detail_scroll: 0,
            toast: None,
            filter: String::new(),
            filter_input: false,
            list_area: Rect::default(),
            detail_area: Rect::default(),
        };
        state.set_prs(prs);
        state
    }

    pub fn set_prs(&mut self, prs: Vec<PullRequest>) {
        self.groups = group_by_repo(prs);
        self.rebuild_rows();
    }

    /// Rebuilds the visible rows from the full PR data, applying the repo filter.
    /// Filtering only hides rows, so clearing it brings every PR back.
    fn rebuild_rows(&mut self) {
        self.prs.clear();
        self.rows.clear();
        self.list_state = ListState::default();

        let needle = self.filter.trim().to_lowercase();
        for group in &self.groups {
            if !needle.is_empty() && !group.repo.to_lowercase().contains(&needle) {
                continue;
            }
            self.rows.push(Row::Header {
                repo: group.repo.clone(),
                count: group.prs.len(),
            });
            for pr in &group.prs {
                self.rows.push(Row::Pr(self.prs.len()));
                self.prs.push(pr.clone());
            }
        }

        self.selected_row = self
            .rows
            .iter()
            .position(|row| matches!(row, Row::Pr(_)))
            .unwrap_or(0);
        self.detail_scroll = 0;
    }

    pub fn filter_push(&mut self, character: char) {
        self.filter.push(character);
        self.rebuild_rows();
    }

    pub fn filter_pop(&mut self) {
        if self.filter.pop().is_some() {
            self.rebuild_rows();
        }
    }

    pub fn clear_filter(&mut self) {
        if !self.filter.is_empty() {
            self.filter.clear();
            self.rebuild_rows();
        }
    }

    pub fn notify(&mut self, message: impl Into<String>) {
        self.toast = Some(Toast {
            message: message.into(),
            expires: Instant::now() + TOAST_LIFETIME,
        });
    }

    pub fn expire_toast(&mut self) {
        if self
            .toast
            .as_ref()
            .is_some_and(|toast| toast.expires <= Instant::now())
        {
            self.toast = None;
        }
    }

    pub fn selected_pr(&self) -> Option<&PullRequest> {
        match self.rows.get(self.selected_row)? {
            Row::Pr(index) => self.prs.get(*index),
            Row::Header { .. } => None,
        }
    }

    pub fn review_status(&self, pr: &PullRequest) -> Option<&ReviewStatus> {
        self.reviews.get(&pr.url)
    }

    pub fn begin_review(&mut self) -> Option<PullRequest> {
        if self.active.is_some() {
            return None;
        }
        let pr = self.selected_pr()?.clone();
        self.active = Some(pr.url.clone());
        self.reviews
            .insert(pr.url.clone(), ReviewStatus::Running(Stage::FetchingDiff));
        self.detail_scroll = 0;
        Some(pr)
    }

    pub fn spawn_review(&mut self, tx: &Sender<ReviewEvent>) {
        if let Some(pr) = self.begin_review() {
            crate::review::spawn(pr, tx.clone());
        }
    }

    pub fn apply(&mut self, event: ReviewEvent) {
        match event {
            ReviewEvent::Stage { url, stage } => {
                if self.active.as_deref() == Some(url.as_str()) {
                    self.reviews.insert(url, ReviewStatus::Running(stage));
                }
            }
            ReviewEvent::Done { url, report } => {
                if self.active.as_deref() == Some(url.as_str()) {
                    self.active = None;
                }
                self.reviews.insert(url, ReviewStatus::Done(report));
                self.detail_scroll = 0;
            }
            ReviewEvent::Failed { url, error } => {
                if self.active.as_deref() == Some(url.as_str()) {
                    self.active = None;
                }
                self.reviews
                    .insert(url, ReviewStatus::Failed(error.clone()));
                self.notify(format!("review failed: {error}"));
                self.detail_scroll = 0;
            }
        }
    }

    pub fn select_next(&mut self) {
        if let Some(index) =
            (self.selected_row + 1..self.rows.len()).find(|index| self.is_pr_row(*index))
        {
            self.set_selected(index);
        }
    }

    pub fn select_previous(&mut self) {
        if let Some(index) = (0..self.selected_row)
            .rev()
            .find(|index| self.is_pr_row(*index))
        {
            self.set_selected(index);
        }
    }

    pub fn select_first(&mut self) {
        if let Some(index) = (0..self.rows.len()).find(|index| self.is_pr_row(*index)) {
            self.set_selected(index);
        }
    }

    pub fn select_last(&mut self) {
        if let Some(index) = (0..self.rows.len())
            .rev()
            .find(|index| self.is_pr_row(*index))
        {
            self.set_selected(index);
        }
    }

    pub fn scroll_detail(&mut self, delta: i32) {
        self.detail_scroll = self.detail_scroll.saturating_add_signed(delta as i16);
    }

    fn set_selected(&mut self, index: usize) {
        if index != self.selected_row {
            self.selected_row = index;
            self.detail_scroll = 0;
        }
    }

    fn is_pr_row(&self, index: usize) -> bool {
        matches!(self.rows.get(index), Some(Row::Pr(_)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{Author, Repository};

    fn pr(number: u64, repo: &str) -> PullRequest {
        PullRequest {
            number,
            title: format!("PR {number}"),
            url: format!("https://github.com/{repo}/pull/{number}"),
            body: String::new(),
            repository: Repository {
                name_with_owner: repo.to_string(),
            },
            author: Author::default(),
            is_draft: false,
            updated_at: String::new(),
        }
    }

    #[test]
    fn repo_filter_hides_rows_without_dropping_prs() {
        let mut state = State::new(vec![pr(1, "acme/api"), pr(2, "acme/web")]);
        for character in "WEB".chars() {
            state.filter_push(character);
        }
        assert_eq!(state.prs.len(), 1);
        assert_eq!(state.prs[0].repo_name(), "acme/web");

        state.clear_filter();
        assert_eq!(state.prs.len(), 2);
        assert_eq!(state.rows.len(), 4);
    }
}
