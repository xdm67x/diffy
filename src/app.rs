use crate::api::{self, ReviewAction};
use crate::models::{Finding, PullRequest, ReviewReport, Verdict};
use crate::review::ReviewEvent;
use crate::state::{ReviewStatus, Row, State};
use crate::store;
use crate::time;

use gpui_kit::component::{
    ActiveTheme, Disableable, Icon, IconName, Sizable, WindowExt,
    button::{Button, ButtonVariant, ButtonVariants},
    h_flex,
    input::{Escape as InputEscape, Input, InputEvent, InputState},
    link::Link,
    list::ListItem,
    scroll::ScrollableElement,
    spinner::Spinner,
    status_bar::StatusBar,
    tag::Tag,
    text::TextView,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

const CONTEXT: &str = "Diffy";
/// While the filter has focus, letter keys belong to the text field, so this
/// context only keeps escape and the modifier shortcut for the sidebar.
const FILTER_CONTEXT: &str = "DiffyFilter";

actions!(diffy, [ToggleSidebar, RefreshPrs, FocusFilter, LeaveFilter]);

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-b", ToggleSidebar, Some(CONTEXT)),
        KeyBinding::new("cmd-f", FocusFilter, Some(CONTEXT)),
        KeyBinding::new("cmd-r", RefreshPrs, Some(CONTEXT)),
        KeyBinding::new("escape", LeaveFilter, Some(FILTER_CONTEXT)),
        KeyBinding::new("cmd-b", ToggleSidebar, Some(FILTER_CONTEXT)),
        KeyBinding::new("cmd-r", RefreshPrs, Some(FILTER_CONTEXT)),
    ]);
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Tone {
    Info,
    Success,
    Error,
}

struct Notice {
    text: String,
    tone: Tone,
}

pub struct Diffy {
    state: State,
    focus_handle: FocusHandle,
    filter: Entity<InputState>,
    sidebar_visible: bool,
    list_scroll: ScrollHandle,
    detail_scroll: ScrollHandle,
    loading: bool,
    posting: Option<ReviewAction>,
    notice: Option<Notice>,
    /// When each running review started, keyed by PR url.
    started: HashMap<String, Instant>,
    events: Sender<ReviewEvent>,
    _subscriptions: Vec<Subscription>,
}

impl Diffy {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder("Filter pull requests"));
        let subscription =
            cx.subscribe_in(
                &filter,
                window,
                |this, state, event, window, cx| match event {
                    InputEvent::Change => {
                        let value = state.read(cx).value().to_string();
                        this.state.set_filter(value);
                        this.reset_selection_view(cx);
                    }
                    InputEvent::PressEnter { .. } => window.focus(&this.focus_handle, cx),
                    InputEvent::Focus | InputEvent::Blur => {}
                },
            );

        let (events, receiver) = mpsc::channel();
        // Review threads report through a channel; drain it on the foreground
        // thread so `State` is only touched from the UI thread.
        cx.spawn(async move |this, cx| {
            let receiver: Receiver<ReviewEvent> = receiver;
            loop {
                let mut batch = Vec::new();
                while let Ok(event) = receiver.try_recv() {
                    batch.push(event);
                }
                if !batch.is_empty()
                    && this
                        .update(cx, |this, cx| {
                            for event in batch {
                                this.apply(event, cx);
                            }
                        })
                        .is_err()
                {
                    break;
                }
                if this.upgrade().is_none() {
                    break;
                }
                cx.background_executor()
                    .timer(Duration::from_millis(100))
                    .await;
            }
        })
        .detach();

        let mut this = Self {
            state: State::new(Vec::new()),
            focus_handle: cx.focus_handle(),
            filter,
            sidebar_visible: true,
            list_scroll: ScrollHandle::new(),
            detail_scroll: ScrollHandle::new(),
            loading: false,
            posting: None,
            notice: None,
            started: HashMap::new(),
            events,
            _subscriptions: vec![subscription],
        };

        match store::load_reviews() {
            Ok(saved) => this
                .state
                .reviews
                .extend(saved.into_iter().map(|(url, reports)| {
                    let history = reports
                        .into_iter()
                        .map(|(reviewed_at, report)| ReviewStatus::Done {
                            report,
                            reviewed_at,
                        })
                        .collect();
                    (url, history)
                })),
            Err(error) => this.set_error(format!("Could not load saved reviews: {error:#}")),
        }
        this
    }

    pub fn focus_handle(&self) -> &FocusHandle {
        &self.focus_handle
    }

    /// Re-reads GitHub in the background; the current list stays usable while it runs.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.loading {
            return;
        }
        self.loading = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx.background_spawn(async { api::get_review_prs() }).await;
            let _ = this.update(cx, |this, cx| {
                this.loading = false;
                match result {
                    Ok(prs) => {
                        this.state.set_prs(prs);
                        this.notice = Some(Notice {
                            text: "Updated just now".to_string(),
                            tone: Tone::Info,
                        });
                        this.reset_selection_view(cx);
                    }
                    Err(error) => {
                        this.set_error(format!("Could not load review requests: {error:#}"))
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn run_review(&mut self, cx: &mut Context<Self>) {
        let Some(pr) = self.state.begin_review() else {
            if self.state.selected_pr().is_some() {
                self.set_error("This PR is already being reviewed.");
                cx.notify();
            }
            return;
        };
        let target = format!("{}#{}", pr.repo_name(), pr.number);
        self.reset_detail_scroll();
        self.started.insert(pr.url.clone(), Instant::now());
        self.notice = Some(Notice {
            text: format!("Reviewing {target}…"),
            tone: Tone::Info,
        });
        cx.notify();

        let events = self.events.clone();
        crate::review::spawn(pr, move |event| {
            let _ = events.send(event);
        });
    }

    fn apply(&mut self, event: ReviewEvent, cx: &mut Context<Self>) {
        let done = match &event {
            ReviewEvent::Done { url, report } => Some((url.clone(), report.clone())),
            _ => None,
        };
        let failed = match &event {
            ReviewEvent::Failed { url, error } => Some((url.clone(), error.clone())),
            _ => None,
        };

        self.state.apply(event);

        if let Some((url, report)) = done {
            self.started.remove(&url);
            if let Err(error) = store::save_review(&url, &report) {
                self.set_error(format!("Could not save review: {error:#}"));
            } else {
                self.notice = Some(Notice {
                    text: format!("Saved the review of {}.", self.state.label(&url)),
                    tone: Tone::Success,
                });
            }
            self.reset_detail_scroll();
        }
        if let Some((url, error)) = failed {
            self.started.remove(&url);
            self.set_error(format!(
                "Review of {} failed: {error}",
                self.state.label(&url)
            ));
            self.reset_detail_scroll();
        }
        cx.notify();
    }

    fn select_url(&mut self, url: &str, cx: &mut Context<Self>) {
        self.state.select_url(url);
        self.reset_selection_view(cx);
    }

    fn reset_selection_view(&mut self, cx: &mut Context<Self>) {
        self.reset_detail_scroll();
        if let Some(row) = self.state.selected_row() {
            self.list_scroll.scroll_to_item(row);
        }
        cx.notify();
    }

    fn reset_detail_scroll(&mut self) {
        self.detail_scroll.set_offset(point(px(0.), px(0.)));
    }

    fn repo_count(&self) -> usize {
        self.state.groups.len()
    }

    fn request_count(&self) -> usize {
        self.state.groups.iter().map(|group| group.prs.len()).sum()
    }

    fn set_error(&mut self, text: impl Into<String>) {
        self.notice = Some(Notice {
            text: text.into(),
            tone: Tone::Error,
        });
    }

    /// Opens the confirmation for posting the shown review. Nothing reaches
    /// GitHub before the dialog's action button is pressed.
    fn confirm_post(&mut self, action: ReviewAction, window: &mut Window, cx: &mut Context<Self>) {
        if self.posting.is_some() {
            return;
        }
        let Some(pr) = self.state.selected_pr().cloned() else {
            self.set_error("Select a pull request first.");
            cx.notify();
            return;
        };
        let Some(ReviewStatus::Done { report, .. }) = self.state.shown_status(&pr).cloned() else {
            self.set_error("Finish a review before posting it.");
            cx.notify();
            return;
        };
        let body = report.to_markdown();
        let target = format!("{}#{}", pr.repo_name(), pr.number);
        let this = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |dialog, _, _| {
            let this = this.clone();
            let pr = pr.clone();
            let body = body.clone();
            dialog
                .title(confirm_title(action))
                .description(format!(
                    "Diffy posts this review to {target} on GitHub as you. The body is the report shown in the window."
                ))
                .confirm()
                .ok_text(action_label(action))
                .ok_variant(action_variant(action))
                .cancel_text("Cancel")
                .on_ok(move |_, _, cx| {
                    let this = this.clone();
                    let pr = pr.clone();
                    let body = body.clone();
                    let _ = this.update(cx, |this, cx| this.post(pr, action, body, cx));
                    true
                })
        });
    }

    fn post(
        &mut self,
        pr: PullRequest,
        action: ReviewAction,
        body: String,
        cx: &mut Context<Self>,
    ) {
        if self.posting.is_some() {
            return;
        }
        self.posting = Some(action);
        cx.notify();
        let target = format!("{}#{}", pr.repo_name(), pr.number);
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move { api::submit_review(&pr, action, &body) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.posting = None;
                match result {
                    Ok(()) => {
                        this.notice = Some(Notice {
                            text: format!("{} {target}", verb(action)),
                            tone: Tone::Success,
                        })
                    }
                    Err(error) => this.set_error(format!("Could not post the review: {error:#}")),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn toggle_sidebar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.sidebar_visible && self.filter_is_focused(window, cx) {
            window.focus(&self.focus_handle, cx);
        }
        self.sidebar_visible = !self.sidebar_visible;
        cx.notify();
    }

    fn filter_is_focused(&self, window: &Window, cx: &App) -> bool {
        self.filter.focus_handle(cx).is_focused(window)
    }

    fn focus_filter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.sidebar_visible {
            self.sidebar_visible = true;
        }
        let handle = self.filter.focus_handle(cx);
        window.focus(&handle, cx);
        cx.notify();
    }

    fn leave_filter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.filter_is_focused(window, cx) {
            window.focus(&self.focus_handle, cx);
            cx.notify();
        }
    }

    fn on_toggle_sidebar(
        &mut self,
        _: &ToggleSidebar,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.toggle_sidebar(window, cx);
    }

    fn on_refresh(&mut self, _: &RefreshPrs, _: &mut Window, cx: &mut Context<Self>) {
        self.refresh(cx);
    }

    fn on_focus_filter(&mut self, _: &FocusFilter, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_filter(window, cx);
    }

    fn on_leave_filter(&mut self, _: &LeaveFilter, window: &mut Window, cx: &mut Context<Self>) {
        self.leave_filter(window, cx);
    }

    /// The text field propagates the escape it cannot use, so the window can
    /// return focus to the PR list.
    fn on_input_escape(&mut self, _: &InputEscape, window: &mut Window, cx: &mut Context<Self>) {
        if self.filter_is_focused(window, cx) {
            self.leave_filter(window, cx);
        } else {
            cx.propagate();
        }
    }

    fn toggle_button(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let hint = if self.sidebar_visible {
            "Hide review requests (⌘B)"
        } else {
            "Show review requests (⌘B)"
        };
        let icon = if self.sidebar_visible {
            IconName::PanelLeftClose
        } else {
            IconName::PanelLeftOpen
        };
        Button::new("toggle-sidebar")
            .ghost()
            .small()
            .icon(icon)
            .tooltip(hint)
            .accessibility_label(hint)
            .on_click(cx.listener(|this, _, window, cx| this.toggle_sidebar(window, cx)))
    }

    fn main_pane(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .h_full()
            .child(self.main_top_bar(cx))
            .child(self.detail(cx))
            .child(self.status_bar(cx))
    }

    /// The strip beside the traffic lights. It keeps the sidebar toggle in one
    /// place whether the list is open or hidden, and gives the window a drag
    /// surface above the content.
    fn main_top_bar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .h_9()
            .flex_shrink_0()
            .px_4()
            .items_center()
            .window_control_area(WindowControlArea::Drag)
            .when(!self.sidebar_visible, |this| {
                // Clear the traffic lights before the toggle when the list is
                // hidden and the main pane reaches the window's left edge.
                this.pl_20().child(self.toggle_button(cx))
            })
    }

    fn sidebar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut list = v_flex()
            .id("pr-list")
            .flex_1()
            .min_h_0()
            .py_2()
            .track_scroll(&self.list_scroll)
            .overflow_y_scrollbar();

        let rows = self.state.rows.clone();
        for row in rows {
            match row {
                Row::Header { repo, count } => {
                    list = list.child(self.repo_header(&repo, count, cx));
                }
                Row::Pr(index) => {
                    if let Some(pr) = self.state.prs.get(index).cloned() {
                        list = list.child(self.pr_row(&pr, cx));
                    }
                }
            }
        }

        let visible = self.state.prs.len();
        let requests = self.request_count();
        let repos = self.repo_count();
        let active = self.state.active.len();

        v_flex()
            .w_80()
            .h_full()
            .flex_shrink_0()
            .border_r_1()
            .border_color(cx.theme().sidebar_border)
            .bg(cx.theme().sidebar)
            .child(
                h_flex()
                    .h_9()
                    .flex_shrink_0()
                    .pl_20()
                    .pr_3()
                    .items_center()
                    .window_control_area(WindowControlArea::Drag)
                    .child(self.toggle_button(cx))
                    .child(div().flex_1()),
            )
            .child(
                v_flex().px_3().pb_3().child(
                    Input::new(&self.filter)
                        .id("repo-filter")
                        .aria_label("Filter review requests")
                        .bordered(false)
                        .cleanable(true)
                        .prefix(Icon::new(IconName::Search).small())
                        .suffix(
                            div()
                                .px_1p5()
                                .py_0p5()
                                .rounded_sm()
                                .border_1()
                                .border_color(cx.theme().border)
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child("⌘F"),
                        ),
                ),
            )
            .child(
                h_flex()
                    .px_4()
                    .pt_2()
                    .pb_1()
                    .gap_2()
                    .items_center()
                    .child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(cx.theme().muted_foreground)
                            .child("Review requests"),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(visible.to_string()),
                    )
                    .child(div().flex_1())
                    .child(
                        Button::new("refresh")
                            .ghost()
                            .small()
                            .icon(IconName::RefreshCw)
                            .tooltip("Reload review requests (⌘R)")
                            .loading(self.loading)
                            .disabled(self.loading)
                            .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
                    ),
            )
            .child(list)
            .child(
                h_flex()
                    .h_9()
                    .flex_shrink_0()
                    .px_4()
                    .gap_2()
                    .items_center()
                    .border_t_1()
                    .border_color(cx.theme().sidebar_border)
                    .child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(cx.theme().sidebar_foreground)
                            .child("Diffy"),
                    )
                    .child(div().flex_1())
                    .when(active > 0, |this| {
                        this.child(
                            h_flex()
                                .gap_2()
                                .text_xs()
                                .text_color(cx.theme().warning)
                                .child(Spinner::new().small())
                                .child(format!("{active} review{} running", plural(active))),
                        )
                    })
                    .when(active == 0, |this| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(format!(
                                    "{requests} request{} · {repos} repo{}",
                                    plural(requests),
                                    plural(repos)
                                )),
                        )
                    }),
            )
    }

    fn repo_header(&self, repo: &str, count: usize, cx: &Context<Self>) -> impl IntoElement {
        h_flex()
            .px_3()
            .pt_3()
            .pb_1()
            .gap_2()
            .child(
                div()
                    .text_xs()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(cx.theme().sidebar_foreground)
                    .child(repo.to_string()),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(count.to_string()),
            )
    }

    fn pr_row(&self, pr: &PullRequest, cx: &mut Context<Self>) -> impl IntoElement {
        let (icon, color, status) = self.marker(&pr.url, cx);
        let selected = self.state.selected.as_deref() == Some(pr.url.as_str());
        let stale = self.review_is_stale(pr);
        let meta = pr_meta(pr);
        let url = pr.url.clone();
        let mut label = format!("PR #{} {} — {}", pr.number, pr.title, status);
        if !meta.is_empty() {
            label.push_str(&format!("; {meta}"));
        }
        if stale {
            label.push_str("; the review is out of date");
        }
        ListItem::new(ElementId::Name(format!("pr-{}", pr.url).into()))
            .selected(selected)
            .accessibility_label(label)
            .on_click(cx.listener(move |this, _, _, cx| this.select_url(&url, cx)))
            .child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .items_start()
                    .child(
                        div()
                            .w_4()
                            .mt_0p5()
                            .flex_shrink_0()
                            .flex()
                            .justify_center()
                            .child(Icon::new(icon).small().text_color(color)),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_1()
                            .text_sm()
                            .child(
                                div()
                                    .w_full()
                                    .min_w_0()
                                    .line_clamp(2)
                                    .child(pr.title.clone()),
                            )
                            .child(
                                h_flex()
                                    .w_full()
                                    .gap_2()
                                    .items_center()
                                    .text_xs()
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .truncate()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(meta),
                                    )
                                    .when(stale, |this| {
                                        this.child(
                                            div()
                                                .flex_shrink_0()
                                                .text_color(cx.theme().warning)
                                                .child("out of date"),
                                        )
                                    })
                                    .child(
                                        div()
                                            .flex_shrink_0()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(format!("#{}", pr.number)),
                                    ),
                            ),
                    ),
            )
    }

    /// True when the newest finished review predates the latest push, so the
    /// verdict no longer covers everything in the PR.
    fn review_is_stale(&self, pr: &PullRequest) -> bool {
        let Some(ReviewStatus::Done { reviewed_at, .. }) = self
            .state
            .reviews
            .get(&pr.url)
            .and_then(|history| history.last())
        else {
            return false;
        };
        pr.updated_after(*reviewed_at)
    }

    fn marker(&self, url: &str, cx: &App) -> (IconName, Hsla, String) {
        match self
            .state
            .reviews
            .get(url)
            .and_then(|history| history.last())
        {
            None => (
                IconName::Minus,
                cx.theme().muted_foreground,
                "not reviewed".to_string(),
            ),
            Some(ReviewStatus::Running(stage)) => (
                IconName::Loader,
                cx.theme().warning,
                self.state
                    .progress
                    .get(url)
                    .cloned()
                    .unwrap_or_else(|| stage.label().to_string()),
            ),
            Some(ReviewStatus::Done { report, .. }) => match report.verdict {
                Verdict::Merge => (
                    IconName::CircleCheck,
                    cx.theme().success,
                    "merge".to_string(),
                ),
                Verdict::RequestChanges => (
                    IconName::CircleX,
                    cx.theme().danger,
                    "changes requested".to_string(),
                ),
            },
            Some(ReviewStatus::Failed(_)) => (
                IconName::CircleAlert,
                cx.theme().danger,
                "review failed".to_string(),
            ),
        }
    }

    fn detail(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(pr) = self.state.selected_pr().cloned() else {
            return v_flex()
                .flex_1()
                .min_w_0()
                .min_h_0()
                .child(self.empty_detail(cx));
        };
        let history_len = self.state.review_history(&pr).len();
        v_flex()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .child(self.detail_header(&pr, history_len, cx))
            .child(
                div()
                    .id("review-scroll")
                    .flex_1()
                    .min_h_0()
                    .track_scroll(&self.detail_scroll)
                    .overflow_y_scrollbar()
                    .child(
                        v_flex()
                            .w_full()
                            .max_w(rems(40.))
                            .mx_auto()
                            .p_6()
                            .gap_6()
                            .child(self.review(&pr, cx))
                            .child(self.description(&pr, cx)),
                    ),
            )
    }

    fn empty_detail(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let loading = self.loading;
        let filtered = !self.state.filter.trim().is_empty();
        let (title, body) = if loading {
            (
                "Loading review requests",
                "Fetching open PRs where you or your team is requested.",
            )
        } else if filtered {
            (
                "No pull request matches this filter",
                "The filter searches the repository, the title, the author, and the number.",
            )
        } else {
            (
                "No review requests",
                "Open pull requests where you or your team is asked to review appear here.",
            )
        };

        v_flex()
            .size_full()
            .justify_center()
            .items_center()
            .gap_4()
            .p_6()
            .child(
                div()
                    .size_12()
                    .rounded_lg()
                    .bg(cx.theme().muted)
                    .flex()
                    .items_center()
                    .justify_center()
                    .when(loading, |this| this.child(Spinner::new().large()))
                    .when(!loading, |this| {
                        this.child(
                            Icon::new(IconName::Inbox)
                                .large()
                                .text_color(cx.theme().muted_foreground),
                        )
                    }),
            )
            .child(
                v_flex()
                    .gap_1()
                    .items_center()
                    .max_w_96()
                    .child(
                        div()
                            .text_lg()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(cx.theme().foreground)
                            .child(title),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_center()
                            .text_color(cx.theme().muted_foreground)
                            .child(body),
                    ),
            )
            .when(!loading && filtered, |this| {
                this.child(
                    Button::new("clear-filter-empty")
                        .label("Clear filter")
                        .outline()
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.state.clear_filter();
                            this.reset_selection_view(cx);
                        })),
                )
            })
            .when(!loading && !filtered, |this| {
                this.child(
                    Button::new("refresh-empty")
                        .label("Refresh")
                        .outline()
                        .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
                )
            })
    }

    fn detail_header(
        &mut self,
        pr: &PullRequest,
        history_len: usize,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let running = self.state.active.contains(&pr.url);
        let shown = self.state.shown_status(pr).cloned();
        let done = matches!(shown, Some(ReviewStatus::Done { .. }));
        let stale = self.review_is_stale(pr);
        let busy = self.posting.is_some();
        let mut header = v_flex().gap_3();

        let mut meta = vec![format!("by {}", author_name(pr))];
        if let Some(updated) = time::parse_utc(&pr.updated_at) {
            meta.push(format!("updated {}", time::ago(updated, time::now())));
        }
        if pr.comments_count > 0 {
            meta.push(format!(
                "{} comment{}",
                pr.comments_count,
                plural(pr.comments_count as usize)
            ));
        }

        header = header.child(
            h_flex()
                .gap_2()
                .items_center()
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(format!("#{} · {}", pr.number, pr.repo_name())),
                )
                .child(div().flex_1())
                .child(
                    Link::new(ElementId::Name(format!("open-{}", pr.url).into()))
                        .href(pr.url.clone())
                        .text_xs()
                        .child(
                            h_flex()
                                .gap_1()
                                .child(Icon::new(IconName::ExternalLink).small())
                                .child("Open on GitHub"),
                        ),
                ),
        );

        header = header.child(
            h_flex()
                .gap_3()
                .items_start()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_2xl()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(pr.title.clone()),
                )
                .when(pr.is_draft, |this| {
                    this.child(Tag::warning().child("Draft"))
                }),
        );

        header = header.child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(meta.join(" · ")),
        );

        header = header.child(
            h_flex()
                .gap_2()
                .items_center()
                .child(
                    Button::new("run-review")
                        .label(if running {
                            "Reviewing…"
                        } else if history_len > 0 {
                            "Run review again"
                        } else {
                            "Run review"
                        })
                        .icon(IconName::Play)
                        // Running is the main action until a review exists; once a
                        // review is ready, posting it is what the reader came for.
                        .when(!running && (!done || stale), |this| this.primary())
                        .when(!running && done && !stale, |this| this.outline())
                        .tooltip("Run the reviewer council")
                        .loading(running)
                        .disabled(running)
                        .on_click(cx.listener(|this, _, _, cx| this.run_review(cx))),
                )
                .when(!running && done, |this| {
                    this.child(div().flex_1())
                        .child(
                            Button::new("post-comment")
                                .label("Comment")
                                .outline()
                                .disabled(busy)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.confirm_post(ReviewAction::Comment, window, cx)
                                })),
                        )
                        .child(
                            Button::new("post-changes")
                                .label("Request changes")
                                .danger()
                                .disabled(busy)
                                .loading(self.posting == Some(ReviewAction::RequestChanges))
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.confirm_post(ReviewAction::RequestChanges, window, cx)
                                })),
                        )
                        .child(
                            Button::new("post-approve")
                                .label("Approve")
                                .success()
                                .disabled(busy)
                                .loading(self.posting == Some(ReviewAction::Approve))
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.confirm_post(ReviewAction::Approve, window, cx)
                                })),
                        )
                }),
        );

        v_flex()
            .flex_shrink_0()
            .p_6()
            .bg(cx.theme().sidebar)
            .border_b_1()
            .border_color(cx.theme().sidebar_border)
            .child(v_flex().w_full().max_w(rems(40.)).mx_auto().child(header))
    }

    fn description(&mut self, pr: &PullRequest, cx: &Context<Self>) -> impl IntoElement {
        v_flex()
            .gap_3()
            .p_4()
            .rounded_lg()
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().popover)
            .child(section_title("Description", cx))
            .child(
                TextView::markdown(
                    ElementId::Name(format!("description-{}", pr.url).into()),
                    pr.description(),
                )
                .selectable(true),
            )
    }

    fn review(&mut self, pr: &PullRequest, cx: &mut Context<Self>) -> impl IntoElement {
        let mut section = v_flex().gap_3().child(section_title("Review", cx));
        match self.state.shown_status(pr).cloned() {
            None => {
                section = section
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child("Not reviewed yet. Run the reviewer council to get a verdict."),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(
                                "The reviewer council runs two independent reviewer agents in \
                                 parallel with gh/git access to inspect the PR branch, then a judge \
                                 confirms what both found, rules on the rest, and checks the diff \
                                 against the PR description.",
                            ),
                    );
            }
            Some(ReviewStatus::Running(stage)) => {
                section = section.child(
                    v_flex()
                        .gap_2()
                        .child(
                            h_flex()
                                .gap_2()
                                .items_center()
                                .child(Spinner::new().small().color(cx.theme().warning))
                                .child(
                                    div()
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(cx.theme().warning)
                                        .child(stage.label()),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(format!(
                                            "{}s",
                                            self.started
                                                .get(&pr.url)
                                                .map(|started| started.elapsed().as_secs())
                                                .unwrap_or_default()
                                        )),
                                ),
                        )
                        .when_some(self.state.progress.get(&pr.url).cloned(), |this, line| {
                            this.child(
                                div()
                                    .text_xs()
                                    .font_family(cx.theme().mono_font_family.clone())
                                    .text_color(cx.theme().muted_foreground)
                                    .child(line),
                            )
                        })
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(
                                    "Claude is working. The review stays here when it finishes, \
                                     even if you move to another PR.",
                                ),
                        ),
                );
            }
            Some(ReviewStatus::Failed(error)) => {
                section = section.child(
                    v_flex()
                        .gap_2()
                        .p_3()
                        .rounded_lg()
                        .border_1()
                        .border_color(cx.theme().danger)
                        .child(
                            h_flex()
                                .gap_2()
                                .items_center()
                                .child(
                                    Icon::new(IconName::CircleAlert)
                                        .small()
                                        .text_color(cx.theme().danger),
                                )
                                .child(
                                    div()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(cx.theme().danger)
                                        .child("Review failed"),
                                ),
                        )
                        .child(
                            div()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child(error),
                        ),
                );
            }
            Some(ReviewStatus::Done {
                report,
                reviewed_at,
            }) => {
                section = section.child(self.report(&report, reviewed_at, pr, cx));
            }
        }
        section
    }

    fn report(
        &mut self,
        report: &ReviewReport,
        reviewed_at: u64,
        pr: &PullRequest,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let history_len = self.state.review_history(pr).len();
        let offset = self.state.review_offset;
        let index = history_len.saturating_sub(1 + offset);
        let findings = ordered_findings(report);
        let counts = severity_summary(&findings);
        let stale = self.review_is_stale(pr);
        let updated_ago =
            time::parse_utc(&pr.updated_at).map(|updated| time::ago(updated, time::now()));

        let verdict_color = match report.verdict {
            Verdict::Merge => cx.theme().success,
            Verdict::RequestChanges => cx.theme().danger,
        };
        let verdict_icon = match report.verdict {
            Verdict::Merge => IconName::CircleCheck,
            Verdict::RequestChanges => IconName::CircleX,
        };

        v_flex()
            .gap_6()
            .child(
                v_flex()
                    .gap_3()
                    .p_4()
                    .rounded_lg()
                    .border_1()
                    .border_color(verdict_color.opacity(0.28))
                    .bg(verdict_color.opacity(0.07))
                    .child(
                        h_flex()
                            .gap_3()
                            .items_center()
                            .child(Icon::new(verdict_icon).small().text_color(verdict_color))
                            .child(
                                div()
                                    .text_lg()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(verdict_color)
                                    .child(verdict_label(report.verdict)),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(format!(
                                        "reviewed {}",
                                        time::ago(reviewed_at as i64, time::now())
                                    )),
                            )
                            .child(div().flex_1())
                            .when(history_len > 1, |this| {
                                this.child(
                                    div()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(format!("Review {} of {}", index + 1, history_len)),
                                )
                                .child(
                                    Button::new("older-review")
                                        .ghost()
                                        .small()
                                        .icon(IconName::ChevronLeft)
                                        .tooltip("Older review")
                                        .disabled(offset + 1 >= history_len)
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.state.older_review();
                                            this.reset_detail_scroll();
                                            cx.notify();
                                        })),
                                )
                                .child(
                                    Button::new("newer-review")
                                        .ghost()
                                        .small()
                                        .icon(IconName::ChevronRight)
                                        .tooltip("Newer review")
                                        .disabled(offset == 0)
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.state.newer_review();
                                            this.reset_detail_scroll();
                                            cx.notify();
                                        })),
                                )
                            }),
                    )
                    .when(stale, |this| {
                        this.child(
                            h_flex()
                                .gap_2()
                                .items_center()
                                .text_xs()
                                .text_color(cx.theme().warning)
                                .child(Icon::new(IconName::CircleAlert).small())
                                .child(match updated_ago {
                                    Some(ago) => format!(
                                        "The PR changed {ago}, after this review, so these findings may be out of date."
                                    ),
                                    None => "The PR changed after this review, so these findings may be out of date."
                                        .to_string(),
                                }),
                        )
                    }),
            )
            .when(!report.intent.trim().is_empty(), |this| {
                this.child(
                    v_flex()
                        .gap_1()
                        .child(section_title("Intent", cx))
                        .child(div().child(report.intent.clone())),
                )
            })
            .when(!report.summary.trim().is_empty(), |this| {
                this.child(
                    v_flex()
                        .gap_1()
                        .child(section_title("Summary", cx))
                        .child(div().child(report.summary.clone())),
                )
            })
            .when(!findings.is_empty(), |this| {
                this.child(
                    v_flex()
                        .gap_2()
                        .child(
                            h_flex()
                                .gap_2()
                                .items_center()
                                .child(section_title(
                                    format!("Findings ({})", findings.len()),
                                    cx,
                                ))
                                .child(div().flex_1())
                                .when(!counts.is_empty(), |this| {
                                    this.child(
                                        div()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(counts),
                                    )
                                }),
                        )
                        .children(
                            findings
                                .iter()
                                .map(|(finding, source)| finding_card(finding, source, cx)),
                        ),
                )
            })
            .when(findings.is_empty(), |this| {
                this.child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .text_sm()
                        .text_color(cx.theme().success)
                        .child(Icon::new(IconName::CircleCheck).small())
                        .child("No actionable findings were reported."),
                )
            })
            .when(!report.dismissed.is_empty(), |this| {
                this.child(
                    v_flex()
                        .gap_2()
                        .child(section_title(
                            format!("Filtered out ({})", report.dismissed.len()),
                            cx,
                        ))
                        .children(report.dismissed.iter().map(|(finding, reason)| {
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(format!(
                                    "• {} — {}{}",
                                    finding.title.trim(),
                                    finding.location(),
                                    if reason.trim().is_empty() {
                                        String::new()
                                    } else {
                                        format!(" — {reason}")
                                    }
                                ))
                        })),
                )
            })
    }

    fn status_bar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let hints = format!(
            "⌘F filter · ⌘B {} list · ⌘R refresh",
            if self.sidebar_visible { "hide" } else { "show" }
        );
        let position = self
            .state
            .prs
            .iter()
            .position(|pr| Some(pr.url.as_str()) == self.state.selected.as_deref())
            .map(|index| format!("{} of {} · ", index + 1, self.state.prs.len()));
        let mut bar = StatusBar::new()
            .flex_shrink_0()
            .h_9()
            .px_4()
            .border_t_1()
            .border_color(cx.theme().border)
            .text_xs();
        if let Some(position) = position {
            bar = bar.left(position);
        }
        bar.left(hints).right(self.notice_element(cx))
    }

    fn notice_element(&self, cx: &App) -> impl IntoElement {
        let Some(notice) = &self.notice else {
            return div().into_any_element();
        };
        let color = match notice.tone {
            Tone::Info => cx.theme().muted_foreground,
            Tone::Success => cx.theme().success,
            Tone::Error => cx.theme().danger,
        };
        h_flex()
            .gap_1()
            .text_color(color)
            .child(
                Icon::new(match notice.tone {
                    Tone::Info => IconName::Info,
                    Tone::Success => IconName::CircleCheck,
                    Tone::Error => IconName::CircleAlert,
                })
                .small(),
            )
            .child(div().max_w_80().truncate().child(notice.text.clone()))
            .into_any_element()
    }
}

impl Render for Diffy {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let context = if self.filter_is_focused(window, cx) {
            FILTER_CONTEXT
        } else {
            CONTEXT
        };
        v_flex()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .text_sm()
            .track_focus(&self.focus_handle)
            .key_context(context)
            .on_action(cx.listener(Self::on_toggle_sidebar))
            .on_action(cx.listener(Self::on_refresh))
            .on_action(cx.listener(Self::on_focus_filter))
            .on_action(cx.listener(Self::on_leave_filter))
            .on_action(cx.listener(Self::on_input_escape))
            .child(
                h_flex()
                    .items_stretch()
                    .size_full()
                    .when(self.sidebar_visible, |this| this.child(self.sidebar(cx)))
                    .child(self.main_pane(cx)),
            )
    }
}

fn section_title(text: impl Into<SharedString>, cx: &App) -> impl IntoElement {
    div()
        .text_xs()
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(cx.theme().muted_foreground)
        .child(text.into())
}

/// Every finding the report stands behind, worst severity first. Findings both
/// reviewers reported come before judge-accepted ones at the same severity.
fn ordered_findings(report: &ReviewReport) -> Vec<(&Finding, &'static str)> {
    let mut findings: Vec<(&Finding, &'static str)> = report
        .confirmed
        .iter()
        .map(|finding| (finding, "reported by both reviewers"))
        .chain(
            report
                .accepted
                .iter()
                .map(|finding| (finding, "accepted by the judge")),
        )
        .collect();
    findings.sort_by_key(|(finding, source)| {
        (
            finding.severity_rank(),
            *source != "reported by both reviewers",
        )
    });
    findings
}

/// `2 high · 1 medium`, worst first, omitting empty severities.
fn severity_summary(findings: &[(&Finding, &'static str)]) -> String {
    [("high", 0), ("medium", 1), ("low", 2)]
        .into_iter()
        .filter_map(|(label, rank)| {
            let count = findings
                .iter()
                .filter(|(finding, _)| finding.severity_rank() == rank)
                .count();
            (count > 0).then(|| format!("{count} {label}"))
        })
        .collect::<Vec<_>>()
        .join(" · ")
}

fn finding_card(finding: &Finding, source: &str, cx: &App) -> impl IntoElement {
    let severity = finding.severity();
    let tag = match severity.as_str() {
        "critical" | "high" => Tag::danger(),
        "medium" => Tag::warning(),
        _ => Tag::new(),
    };
    v_flex()
        .gap_2()
        .p_4()
        .rounded_lg()
        .border_1()
        .border_color(cx.theme().border)
        .bg(cx.theme().popover)
        .child(
            h_flex()
                .gap_2()
                .items_center()
                .child(tag.small().child(severity.to_uppercase()))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .font_weight(FontWeight::MEDIUM)
                        .child(finding.title.clone()),
                )
                .child(
                    div()
                        .flex_shrink_0()
                        .text_xs()
                        .font_family(cx.theme().mono_font_family.clone())
                        .text_color(cx.theme().muted_foreground)
                        .child(finding.location()),
                ),
        )
        .when(!finding.detail.trim().is_empty(), |this| {
            this.child(div().text_sm().child(finding.detail.trim().to_string()))
        })
        .child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(source.to_string()),
        )
}

fn verdict_label(verdict: Verdict) -> &'static str {
    match verdict {
        Verdict::Merge => "Merge",
        Verdict::RequestChanges => "Request changes",
    }
}

fn action_label(action: ReviewAction) -> &'static str {
    match action {
        ReviewAction::Approve => "Approve",
        ReviewAction::RequestChanges => "Request changes",
        ReviewAction::Comment => "Comment",
    }
}

fn confirm_title(action: ReviewAction) -> &'static str {
    match action {
        ReviewAction::Approve => "Approve this pull request?",
        ReviewAction::RequestChanges => "Request changes on this pull request?",
        ReviewAction::Comment => "Comment on this pull request?",
    }
}

fn action_variant(action: ReviewAction) -> ButtonVariant {
    match action {
        ReviewAction::Approve => ButtonVariant::Success,
        ReviewAction::RequestChanges => ButtonVariant::Danger,
        ReviewAction::Comment => ButtonVariant::Primary,
    }
}

fn verb(action: ReviewAction) -> &'static str {
    match action {
        ReviewAction::Approve => "Approved",
        ReviewAction::RequestChanges => "Requested changes on",
        ReviewAction::Comment => "Commented on",
    }
}

/// Second line of a list row: author, freshness, discussion.
fn pr_meta(pr: &PullRequest) -> String {
    let mut parts = Vec::new();
    if !pr.author.login.is_empty() {
        parts.push(pr.author.login.clone());
    }
    if let Some(updated) = time::parse_utc(&pr.updated_at) {
        parts.push(time::ago(updated, time::now()));
    }
    if pr.is_draft {
        parts.push("draft".to_string());
    }
    parts.join(" · ")
}

fn author_name(pr: &PullRequest) -> &str {
    if pr.author.login.is_empty() {
        "unknown"
    } else {
        &pr.author.login
    }
}

fn plural(count: usize) -> &'static str {
    if count == 1 { "" } else { "s" }
}

#[cfg(test)]
mod tests {
    use super::{Diffy, bind_keys};
    use crate::models::{Author, PullRequest, Repository};
    use gpui_kit::test::TestWindowExt;
    use gpui_kit::{
        AnyWindowHandle, AppContext as _, Bounds, Entity, Focusable as _, TestAppContext,
        WindowBounds, WindowOptions, point, px, size,
    };

    use crate::models::{Finding, ReviewReport, Verdict};

    fn pr(repo: &str, number: u64, title: &str) -> PullRequest {
        PullRequest {
            number,
            title: title.to_string(),
            url: format!("https://github.com/{repo}/pull/{number}"),
            body: String::new(),
            repository: Repository {
                name_with_owner: repo.to_string(),
            },
            author: Author::default(),
            is_draft: false,
            updated_at: String::new(),
            comments_count: 0,
        }
    }

    fn open_app(
        cx: &mut TestAppContext,
        prs: Vec<PullRequest>,
    ) -> (AnyWindowHandle, Entity<Diffy>) {
        let mut built = None;
        let handle = cx.update(|cx| {
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                    point(px(0.), px(0.)),
                    size(px(1100.), px(720.)),
                ))),
                ..Default::default()
            };
            let (handle, _) = gpui_kit::open_window(options, cx, |window, cx| {
                let view = cx.new(|cx| {
                    let mut diffy = Diffy::new(window, cx);
                    diffy.state.set_prs(prs);
                    diffy
                });
                built = Some(view.clone());
                view
            })
            .expect("test window");
            handle
        });
        (handle, built.unwrap())
    }

    fn focus_window(view: &Entity<Diffy>, window: &mut gpui_kit::Window, cx: &mut gpui_kit::App) {
        let focus = view.read(cx).focus_handle.clone();
        window.focus(&focus, cx);
    }

    #[test]
    fn findings_read_worst_first_and_counted_by_severity() {
        let finding = |severity: &str| Finding {
            severity: severity.to_string(),
            ..Finding::default()
        };
        let report = ReviewReport {
            verdict: Verdict::RequestChanges,
            summary: String::new(),
            intent: String::new(),
            confirmed: vec![finding("low")],
            accepted: vec![finding("high"), finding("medium")],
            dismissed: Vec::new(),
        };

        let ordered = super::ordered_findings(&report);
        let severities: Vec<String> = ordered
            .iter()
            .map(|(finding, _)| finding.severity())
            .collect();
        assert_eq!(severities, ["high", "medium", "low"]);
        assert_eq!(
            super::severity_summary(&ordered),
            "1 high · 1 medium · 1 low"
        );
    }

    #[gpui_kit::test]
    fn the_sidebar_toggles_from_the_keyboard(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        cx.update(bind_keys);
        let (handle, view) = open_app(cx, vec![pr("acme/api", 1, "one"), pr("acme/api", 2, "two")]);

        cx.update_window(handle, |_, window, cx| {
            focus_window(&view, window, cx);
            window.render_frame(cx);
            assert_eq!(
                view.read(cx).state.selected_pr().map(|pr| pr.number),
                Some(2)
            );

            window.press("cmd-b", cx);
            assert!(!view.read(cx).sidebar_visible);
            window.press("cmd-b", cx);
            assert!(view.read(cx).sidebar_visible);
        })
        .unwrap();
    }

    #[gpui_kit::test]
    fn typing_a_filter_narrows_the_list_and_escape_returns_to_it(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        cx.update(bind_keys);
        let (handle, view) = open_app(cx, vec![pr("acme/api", 1, "one"), pr("acme/web", 2, "two")]);

        cx.update_window(handle, |_, window, cx| {
            focus_window(&view, window, cx);
            window.render_frame(cx);
            window.press("cmd-f", cx);
            window.render_frame(cx);
            let filter_focus = view.read(cx).filter.focus_handle(cx);
            assert!(
                filter_focus.is_focused(window),
                "the filter shortcut did not focus the field"
            );
        })
        .unwrap();

        cx.update_window(handle, |_, window, cx| window.input("web", cx))
            .unwrap();

        // Input events reach the view after the update that produced them.
        cx.update(|cx| {
            assert_eq!(view.read(cx).filter.read(cx).value().to_string(), "web");
            assert_eq!(view.read(cx).state.filter, "web");
            assert_eq!(
                view.read(cx).state.selected_pr().map(|pr| pr.number),
                Some(2)
            );
            assert!(view.read(cx).state.prs.iter().all(|pr| pr.number == 2));
        });

        cx.update_window(handle, |_, window, cx| {
            window.press("escape", cx);
            window.render_frame(cx);
        })
        .unwrap();

        cx.update_window(handle, |_, window, cx| {
            let filter_focus = view.read(cx).filter.focus_handle(cx);
            assert!(
                !filter_focus.is_focused(window),
                "escape did not return focus to the PR list"
            );
            assert_eq!(view.read(cx).state.filter, "web");
        })
        .unwrap();
    }
}
