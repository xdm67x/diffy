use crate::models::{PrGroup, PullRequest, ReviewReport, group_by_repo};
use crate::review::{ReviewEvent, Stage};
use ratatui::widgets::ListState;
use std::collections::HashMap;
use std::sync::mpsc::Sender;

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

pub struct State {
    pub groups: Vec<PrGroup>,
    pub prs: Vec<PullRequest>,
    pub rows: Vec<Row>,
    pub selected_row: usize,
    pub list_state: ListState,
    pub reviews: HashMap<String, ReviewStatus>,
    pub active: Option<String>,
    pub detail_scroll: u16,
    pub notice: Option<String>,
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
            notice: None,
        };
        state.set_prs(prs);
        state
    }

    pub fn set_prs(&mut self, prs: Vec<PullRequest>) {
        self.groups = group_by_repo(prs);
        self.prs.clear();
        self.rows.clear();
        self.list_state = ListState::default();

        for group in &self.groups {
            self.rows.push(Row::Header {
                repo: group.repo.clone(),
                count: group.prs.len(),
            });
            for pr in &group.prs {
                self.rows.push(Row::Pr(self.prs.len()));
                self.prs.push(pr.clone());
            }
        }

        if !self.is_pr_row(self.selected_row) {
            self.selected_row = self
                .rows
                .iter()
                .position(|row| matches!(row, Row::Pr(_)))
                .unwrap_or(0);
        }
        self.detail_scroll = 0;
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
                self.reviews.insert(url, ReviewStatus::Failed(error));
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
