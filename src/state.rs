use crate::models::{PrGroup, PullRequest, ReviewReport, group_by_repo};
use crate::review::{ReviewEvent, Stage};
use crate::time;
use std::collections::{HashMap, HashSet};

#[derive(Clone, Debug)]
pub enum ReviewStatus {
    Running(Stage),
    Done {
        report: ReviewReport,
        /// Epoch second the review finished, kept so the window can tell a
        /// review that predates the latest push.
        reviewed_at: u64,
    },
    Failed(String),
}

#[derive(Clone, Debug)]
pub enum Row {
    Header { repo: String, count: usize },
    Pr(usize),
}

/// Everything the window renders that is not a GPUI handle: the PR list, the
/// repository filter, selection, and each PR's review history.
#[derive(Default)]
pub struct State {
    pub groups: Vec<PrGroup>,
    pub prs: Vec<PullRequest>,
    pub rows: Vec<Row>,
    /// PR url of the selection; kept by identity so filtering and refreshing keep it.
    pub selected: Option<String>,
    pub reviews: HashMap<String, Vec<ReviewStatus>>,
    /// How many reviews back the review pane shows; 0 is the newest.
    pub review_offset: usize,
    /// PR urls with a review running right now. One review per PR, any number at once.
    pub active: HashSet<String>,
    /// Last thing each running review was seen doing, keyed by PR url.
    pub progress: HashMap<String, String>,
    /// Repository filter, empty means every repo is shown.
    pub filter: String,
}

impl State {
    pub fn new(prs: Vec<PullRequest>) -> Self {
        let mut state = Self::default();
        state.set_prs(prs);
        state
    }

    pub fn set_prs(&mut self, prs: Vec<PullRequest>) {
        self.groups = group_by_repo(prs);
        self.rebuild_rows();
    }

    /// Rebuilds the visible rows from the full PR data, applying the filter to
    /// the repository, title, author, and number. Filtering only hides rows, so
    /// clearing it brings every PR back.
    fn rebuild_rows(&mut self) {
        self.prs.clear();
        self.rows.clear();

        let needle = self.filter.trim().to_lowercase();
        for group in &self.groups {
            let visible: Vec<&PullRequest> = group
                .prs
                .iter()
                .filter(|pr| needle.is_empty() || matches(pr, &needle))
                .collect();
            if visible.is_empty() {
                continue;
            }
            self.rows.push(Row::Header {
                repo: group.repo.clone(),
                count: visible.len(),
            });
            for pr in visible {
                self.rows.push(Row::Pr(self.prs.len()));
                self.prs.push(pr.clone());
            }
        }

        let kept = self
            .selected
            .as_ref()
            .is_some_and(|url| self.prs.iter().any(|pr| &pr.url == url));
        if !kept {
            self.selected = self.prs.first().map(|pr| pr.url.clone());
        }
        self.review_offset = 0;
    }

    pub fn set_filter(&mut self, filter: String) {
        if filter != self.filter {
            self.filter = filter;
            self.rebuild_rows();
        }
    }

    pub fn clear_filter(&mut self) {
        self.set_filter(String::new());
    }

    pub fn selected_pr(&self) -> Option<&PullRequest> {
        let url = self.selected.as_deref()?;
        self.prs.iter().find(|pr| pr.url == url)
    }

    /// Row index of the selection, for keeping it in view.
    pub fn selected_row(&self) -> Option<usize> {
        let url = self.selected.as_deref()?;
        self.rows.iter().position(
            |row| matches!(row, Row::Pr(index) if self.prs.get(*index).is_some_and(|pr| pr.url == url)),
        )
    }

    /// Every review attempt for the PR, oldest first.
    pub fn review_history(&self, pr: &PullRequest) -> &[ReviewStatus] {
        self.reviews.get(&pr.url).map(Vec::as_slice).unwrap_or(&[])
    }

    /// The review currently shown in the review pane.
    pub fn shown_status(&self, pr: &PullRequest) -> Option<&ReviewStatus> {
        let history = self.review_history(pr);
        history.get(history.len().checked_sub(1 + self.review_offset)?)
    }

    pub fn older_review(&mut self) {
        if let Some(pr) = self.selected_pr()
            && self.review_offset + 1 < self.review_history(pr).len()
        {
            self.review_offset += 1;
        }
    }

    pub fn newer_review(&mut self) {
        if self.review_offset > 0 {
            self.review_offset -= 1;
        }
    }

    pub fn select_url(&mut self, url: &str) {
        if self.prs.iter().any(|pr| pr.url == url) {
            self.set_selected(url.to_string());
        }
    }

    fn set_selected(&mut self, url: String) {
        if self.selected.as_deref() != Some(url.as_str()) {
            self.selected = Some(url);
            self.review_offset = 0;
        }
    }

    pub fn label(&self, url: &str) -> String {
        self.prs
            .iter()
            .find(|pr| pr.url == url)
            .map(|pr| format!("{}#{}", pr.repo_name(), pr.number))
            .unwrap_or_else(|| url.to_string())
    }

    /// Starts a review of the selected PR, or returns `None` when it is already running.
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
        self.review_offset = 0;
        Some(pr)
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
                self.settle_review(
                    url.clone(),
                    ReviewStatus::Done {
                        report,
                        reviewed_at: time::now().max(0) as u64,
                    },
                );
                if self.selected.as_deref() == Some(url.as_str()) {
                    self.review_offset = 0;
                }
            }
            ReviewEvent::Failed { url, error } => {
                self.active.remove(&url);
                self.settle_review(url.clone(), ReviewStatus::Failed(error));
                if self.selected.as_deref() == Some(url.as_str()) {
                    self.review_offset = 0;
                }
            }
        }
    }

    /// Replaces the running attempt, or appends when the event has no matching attempt.
    fn settle_review(&mut self, url: String, status: ReviewStatus) {
        self.progress.remove(&url);
        let history = self.reviews.entry(url).or_default();
        if matches!(history.last(), Some(ReviewStatus::Running(_))) {
            history.pop();
        }
        history.push(status);
    }
}

/// True when the filter text belongs to this PR. `needle` is already lowercase.
fn matches(pr: &PullRequest, needle: &str) -> bool {
    pr.repo_name().to_lowercase().contains(needle)
        || pr.title.to_lowercase().contains(needle)
        || pr.author.login.to_lowercase().contains(needle)
        || pr
            .number
            .to_string()
            .contains(needle.trim_start_matches('#'))
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
            by_me: false,
            updated_at: String::new(),
            comments_count: 0,
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
    fn every_review_is_kept_and_the_pane_steps_back() {
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
            Some(ReviewStatus::Done { report, .. }) if report.summary == "two"
        ));

        state.older_review();
        assert!(matches!(
            state.shown_status(&pull),
            Some(ReviewStatus::Done { report, .. }) if report.summary == "one"
        ));

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

        state.select_url("https://github.com/acme/web/pull/2");
        let second = state.selected_pr().unwrap().clone();
        assert_ne!(first.url, second.url);
        assert!(state.begin_review().is_some());
        assert_eq!(state.active.len(), 2);

        finish(&mut state, &first.url, "first done");
        assert_eq!(state.active.len(), 1);
        assert!(state.active.contains(&second.url));
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

        finish(&mut state, &pull.url, "done");
        assert!(
            state.progress.is_empty(),
            "finished reviews keep no activity"
        );
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
    }

    #[test]
    fn repo_filter_hides_rows_without_dropping_prs() {
        let mut state = State::new(vec![pr(1, "acme/api"), pr(2, "acme/web")]);
        state.set_filter("WEB".to_string());
        assert_eq!(state.prs.len(), 1);
        assert_eq!(state.prs[0].repo_name(), "acme/web");

        state.clear_filter();
        assert_eq!(state.prs.len(), 2);
        assert_eq!(state.rows.len(), 4);
    }

    #[test]
    fn the_filter_matches_title_author_and_number() {
        let mut state = State::new(vec![pr(1, "acme/api"), pr(2, "acme/web")]);
        state.groups[0].prs[0].title = "Fix login redirect".to_string();
        state.groups[0].prs[0].author.login = "ada".to_string();
        state.set_prs(
            state
                .groups
                .iter()
                .flat_map(|group| group.prs.clone())
                .collect(),
        );

        state.set_filter("login".to_string());
        assert_eq!(state.prs.len(), 1);
        assert_eq!(state.prs[0].number, 1);

        state.set_filter("ada".to_string());
        assert_eq!(state.prs.len(), 1);

        state.set_filter("#2".to_string());
        assert_eq!(state.prs.len(), 1);
        assert_eq!(state.prs[0].number, 2);
    }

    #[test]
    fn filtering_keeps_the_selection_when_it_is_still_visible() {
        let mut state = State::new(vec![pr(1, "acme/api"), pr(2, "acme/web")]);
        state.select_url("https://github.com/acme/web/pull/2");
        state.set_filter("web".to_string());
        assert_eq!(
            state.selected_pr().map(|pr| pr.number),
            Some(2),
            "the selected PR survives a filter that still shows it"
        );

        state.set_filter("api".to_string());
        assert_eq!(
            state.selected_pr().map(|pr| pr.number),
            Some(1),
            "a filter that hides the selection falls back to a visible PR"
        );
    }
}
