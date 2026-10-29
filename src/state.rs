use crate::api::{self, ReviewAction};
use crate::models::{PrGroup, PullRequest, ReviewReport, group_by_repo};
use crate::review::{ReviewEvent, Stage};
use ratatui::layout::Rect;
use ratatui::widgets::ListState;
use std::collections::{HashMap, HashSet};
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
    pub error: bool,
    expires: Instant,
}

pub struct State {
    pub groups: Vec<PrGroup>,
    pub prs: Vec<PullRequest>,
    pub rows: Vec<Row>,
    pub selected_row: usize,
    pub list_state: ListState,
    pub reviews: HashMap<String, Vec<ReviewStatus>>,
    /// How many reviews back the detail pane shows; 0 is the newest.
    pub review_offset: usize,
    pub submit_mode: bool,
    /// PR urls with a review running right now. One review per PR, any number at once.
    pub active: HashSet<String>,
    /// Last thing each running review was seen doing, keyed by PR url.
    pub progress: HashMap<String, String>,
    /// When each running review started, keyed by PR url.
    pub started: HashMap<String, Instant>,
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
            review_offset: 0,
            submit_mode: false,
            active: HashSet::new(),
            progress: HashMap::new(),
            started: HashMap::new(),
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
        self.review_offset = 0;
        self.submit_mode = false;
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
            error: true,
            expires: Instant::now() + TOAST_LIFETIME,
        });
    }

    pub fn notify_ok(&mut self, message: impl Into<String>) {
        self.toast = Some(Toast {
            message: message.into(),
            error: false,
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

    /// Every review attempt for the PR, oldest first.
    pub fn review_history(&self, pr: &PullRequest) -> &[ReviewStatus] {
        self.reviews.get(&pr.url).map(Vec::as_slice).unwrap_or(&[])
    }

    /// The review currently shown in the detail pane.
    pub fn shown_status(&self, pr: &PullRequest) -> Option<&ReviewStatus> {
        let history = self.review_history(pr);
        history.get(history.len().checked_sub(1 + self.review_offset)?)
    }

    pub fn older_review(&mut self) {
        if let Some(pr) = self.selected_pr()
            && self.review_offset + 1 < self.review_history(pr).len()
        {
            self.review_offset += 1;
            self.submit_mode = false;
            self.detail_scroll = 0;
        }
    }

    pub fn newer_review(&mut self) {
        if self.review_offset > 0 {
            self.review_offset -= 1;
            self.submit_mode = false;
            self.detail_scroll = 0;
        }
    }

    /// Opens the submit prompt when the shown review is a finished one.
    pub fn begin_submit(&mut self) -> bool {
        let ready = match self.selected_pr() {
            Some(pr) => matches!(self.shown_status(pr), Some(ReviewStatus::Done(_))),
            None => false,
        };
        self.submit_mode = ready;
        ready
    }

    /// Posts the shown review to GitHub. Blocking: `gh` takes about a second.
    pub fn submit(&mut self, action: ReviewAction) {
        self.submit_mode = false;
        let Some(pr) = self.selected_pr().cloned() else {
            return;
        };
        let body = match self.shown_status(&pr) {
            Some(ReviewStatus::Done(report)) => report.to_markdown(),
            _ => return,
        };
        match api::submit_review(&pr, action, &body) {
            Ok(()) => self.notify_ok(format!(
                "{} {}#{}",
                action.completed(),
                pr.repo_name(),
                pr.number
            )),
            Err(error) => self.notify(format!("could not post review: {error:#}")),
        }
    }

    pub fn begin_review(&mut self) -> Option<PullRequest> {
        let pr = self.selected_pr()?.clone();
        if !self.active.insert(pr.url.clone()) {
            return None;
        }
        self.reviews
            .entry(pr.url.clone())
            .or_default()
            .push(ReviewStatus::Running(Stage::FetchingDiff));
        self.progress.remove(&pr.url);
        self.started.insert(pr.url.clone(), Instant::now());
        self.review_offset = 0;
        self.submit_mode = false;
        self.detail_scroll = 0;
        Some(pr)
    }

    pub fn spawn_review(&mut self, tx: &Sender<ReviewEvent>) {
        if self.selected_pr().is_none() {
            return;
        }
        match self.begin_review() {
            Some(pr) => crate::review::spawn(pr, tx.clone()),
            None => self.notify("this PR is already being reviewed"),
        }
    }

    pub fn apply(&mut self, event: ReviewEvent) {
        match event {
            ReviewEvent::Stage { url, stage } => {
                if self.active.contains(&url)
                    && let Some(last) = self.reviews.entry(url.clone()).or_default().last_mut()
                {
                    *last = ReviewStatus::Running(stage);
                    self.progress.remove(&url);
                }
            }
            ReviewEvent::Progress { url, text } => {
                if self.active.contains(&url) {
                    self.progress.insert(url, text);
                }
            }
            ReviewEvent::Done { url, report } => {
                self.active.remove(&url);
                let selected = self.selected_pr().is_some_and(|pr| pr.url == url);
                self.settle_review(url, ReviewStatus::Done(report));
                if selected {
                    self.review_offset = 0;
                    self.detail_scroll = 0;
                }
            }
            ReviewEvent::Failed { url, error } => {
                self.active.remove(&url);
                let selected = self.selected_pr().is_some_and(|pr| pr.url == url);
                let label = self.label_for(&url);
                self.settle_review(url, ReviewStatus::Failed(error.clone()));
                self.notify(format!("review of {label} failed: {error}"));
                if selected {
                    self.review_offset = 0;
                    self.detail_scroll = 0;
                }
            }
        }
    }

    fn label_for(&self, url: &str) -> String {
        self.prs
            .iter()
            .find(|pr| pr.url == url)
            .map(|pr| format!("{}#{}", pr.repo_name(), pr.number))
            .unwrap_or_else(|| url.to_string())
    }

    /// Replaces the running attempt, or appends when the event has no matching attempt.
    fn settle_review(&mut self, url: String, status: ReviewStatus) {
        self.progress.remove(&url);
        self.started.remove(&url);
        let history = self.reviews.entry(url).or_default();
        if matches!(history.last(), Some(ReviewStatus::Running(_))) {
            history.pop();
        }
        history.push(status);
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
            self.review_offset = 0;
            self.submit_mode = false;
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
    use crate::models::{Author, Repository, Verdict};

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

    fn report(summary: &str) -> ReviewReport {
        ReviewReport {
            verdict: Verdict::Merge,
            summary: summary.to_string(),
            intent: String::new(),
            confirmed: Vec::new(),
            accepted: Vec::new(),
            dismissed: Vec::new(),
        }
    }

    fn finish(state: &mut State, url: &str, summary: &str) {
        state.apply(ReviewEvent::Done {
            url: url.to_string(),
            report: report(summary),
        });
    }

    #[test]
    fn every_review_is_kept_and_the_detail_pane_steps_back() {
        let mut state = State::new(vec![pr(1, "acme/api")]);
        let pull = state.selected_pr().unwrap().clone();

        assert!(state.begin_review().is_some());
        assert!(matches!(
            state.shown_status(&pull),
            Some(ReviewStatus::Running(_))
        ));
        finish(&mut state, &pull.url, "one");
        assert!(state.begin_review().is_some());
        finish(&mut state, &pull.url, "two");

        assert_eq!(state.review_history(&pull).len(), 2);
        assert!(matches!(
            state.shown_status(&pull),
            Some(ReviewStatus::Done(report)) if report.summary == "two"
        ));

        state.older_review();
        assert!(matches!(
            state.shown_status(&pull),
            Some(ReviewStatus::Done(report)) if report.summary == "one"
        ));
        assert!(state.begin_submit());

        state.older_review();
        assert_eq!(state.review_offset, 1, "cannot step past the oldest review");
        state.newer_review();
        assert_eq!(state.review_offset, 0);
        state.newer_review();
        assert_eq!(state.review_offset, 0, "cannot step past the newest review");
    }

    #[test]
    fn different_prs_review_concurrently_but_a_pr_reviews_once() {
        let mut state = State::new(vec![pr(1, "acme/api"), pr(2, "acme/web")]);
        let first = state.selected_pr().unwrap().clone();
        assert!(state.begin_review().is_some());
        assert!(
            state.begin_review().is_none(),
            "same PR must not start twice"
        );

        state.select_next();
        let second = state.selected_pr().unwrap().clone();
        assert_ne!(first.url, second.url);
        assert!(state.begin_review().is_some());
        assert_eq!(state.active.len(), 2);
        assert!(matches!(
            state.shown_status(&first),
            Some(ReviewStatus::Running(_))
        ));
        assert!(matches!(
            state.shown_status(&second),
            Some(ReviewStatus::Running(_))
        ));

        finish(&mut state, &first.url, "first done");
        assert_eq!(state.active.len(), 1);
        assert!(state.active.contains(&second.url));
        assert!(matches!(
            state.shown_status(&first),
            Some(ReviewStatus::Done(report)) if report.summary == "first done"
        ));
        assert!(matches!(
            state.shown_status(&second),
            Some(ReviewStatus::Running(_))
        ));

        finish(&mut state, &second.url, "second done");
        assert!(state.active.is_empty());
        assert_eq!(state.review_history(&first).len(), 1);
        assert_eq!(state.review_history(&second).len(), 1);
    }

    #[test]
    fn live_progress_follows_the_running_attempt_and_is_cleared() {
        let mut state = State::new(vec![pr(1, "acme/api")]);
        let pull = state.selected_pr().unwrap().clone();
        state.begin_review().unwrap();
        assert!(state.started.contains_key(&pull.url));

        state.apply(ReviewEvent::Progress {
            url: pull.url.clone(),
            text: "reviewer 1 · gh pr diff".to_string(),
        });
        assert_eq!(state.progress[&pull.url], "reviewer 1 · gh pr diff");

        state.apply(ReviewEvent::Stage {
            url: pull.url.clone(),
            stage: Stage::Judging,
        });
        assert!(
            state.progress.is_empty(),
            "a new stage drops the previous activity"
        );

        state.apply(ReviewEvent::Progress {
            url: pull.url.clone(),
            text: "judge · gh pr view".to_string(),
        });
        finish(&mut state, &pull.url, "done");
        assert!(
            state.progress.is_empty(),
            "finished reviews keep no activity"
        );
        assert!(state.started.is_empty());
    }

    #[test]
    fn failed_attempt_replaces_the_running_one() {
        let mut state = State::new(vec![pr(1, "acme/api")]);
        let pull = state.selected_pr().unwrap().clone();
        state.begin_review().unwrap();
        state.apply(ReviewEvent::Failed {
            url: pull.url.clone(),
            error: "gh failed".to_string(),
        });

        let history = state.review_history(&pull);
        assert_eq!(history.len(), 1);
        assert!(matches!(history[0], ReviewStatus::Failed(_)));
        assert!(!state.begin_submit());
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
