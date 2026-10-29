use crate::api::{self, ReviewAction};
use crate::models::{Finding, PullRequest, ReviewReport, Verdict};
use crate::review::ReviewEvent;
use crate::state::{ReviewStatus, Row, State};
use crate::store;

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
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder("Filter repositories"));
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
                    (url, reports.into_iter().map(ReviewStatus::Done).collect())
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
                            text: format!(
                                "{} review request{} across {} repo{}",
                                this.state.prs.len(),
                                plural(this.state.prs.len()),
                                this.repo_count(),
                                plural(this.repo_count())
                            ),
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
        let Some(pr) = self.state.selected_pr().cloned() else {
            return;
        };
        let Some(ReviewStatus::Done(report)) = self.state.shown_status(&pr).cloned() else {
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

    fn toolbar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .h_12()
            .flex_shrink_0()
            .px_3()
            .gap_2()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(self.toggle_button(cx))
            .child(div().font_weight(FontWeight::SEMIBOLD).child("Diffy"))
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!(
                        "{} review request{} · {} repo{}",
                        self.request_count(),
                        plural(self.request_count()),
                        self.repo_count(),
                        plural(self.repo_count())
                    )),
            )
            .child(div().flex_1())
            .when(!self.state.active.is_empty(), |this| {
                this.child(
                    h_flex()
                        .gap_2()
                        .text_xs()
                        .text_color(cx.theme().warning)
                        .child(Spinner::new().small())
                        .child(format!(
                            "{} review{} running",
                            self.state.active.len(),
                            plural(self.state.active.len())
                        )),
                )
            })
            .child(
                Button::new("refresh")
                    .ghost()
                    .small()
                    .icon(IconName::RefreshCw)
                    .label("Refresh")
                    .tooltip("Reload review requests (⌘R)")
                    .loading(self.loading)
                    .disabled(self.loading)
                    .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
            )
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

        if self.state.rows.is_empty() && !self.loading {
            list = list.child(
                v_flex()
                    .p_3()
                    .gap_2()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(if self.state.filter.trim().is_empty() {
                        "No review requests right now."
                    } else {
                        "No repository matches this filter."
                    })
                    .when(!self.state.filter.trim().is_empty(), |this| {
                        this.child(
                            Button::new("clear-filter")
                                .small()
                                .label("Clear filter")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.state.clear_filter();
                                    this.reset_selection_view(cx);
                                })),
                        )
                    }),
            );
        }

        v_flex()
            .w_72()
            .h_full()
            .flex_shrink_0()
            .border_r_1()
            .border_color(cx.theme().sidebar_border)
            .bg(cx.theme().sidebar)
            .child(
                v_flex()
                    .p_3()
                    .gap_2()
                    .border_b_1()
                    .border_color(cx.theme().sidebar_border)
                    .child(
                        h_flex().justify_between().child(
                            div()
                                .text_sm()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(cx.theme().sidebar_foreground)
                                .child("Review requests"),
                        ),
                    )
                    .child(
                        Input::new(&self.filter)
                            .id("repo-filter")
                            .small()
                            .cleanable(true)
                            .prefix(Icon::new(IconName::Search).small()),
                    ),
            )
            .child(list)
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
        let url = pr.url.clone();
        ListItem::new(ElementId::Name(format!("pr-{}", pr.url).into()))
            .selected(selected)
            .accessibility_label(format!("PR #{} {} — {}", pr.number, pr.title, status))
            .on_click(cx.listener(move |this, _, _, cx| this.select_url(&url, cx)))
            .child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .items_center()
                    .child(
                        div()
                            .w_4()
                            .flex_shrink_0()
                            .flex()
                            .justify_center()
                            .child(Icon::new(icon).small().text_color(color)),
                    )
                    .child(div().flex_1().min_w_0().truncate().child(pr.title.clone()))
                    .when(pr.is_draft, |this| {
                        this.child(
                            div()
                                .text_xs()
                                .flex_shrink_0()
                                .text_color(cx.theme().muted_foreground)
                                .child("Draft"),
                        )
                    })
                    .child(
                        div()
                            .text_xs()
                            .flex_shrink_0()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!("#{}", pr.number)),
                    ),
            )
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
            Some(ReviewStatus::Done(report)) => match report.verdict {
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
                .h_full()
                .child(self.empty_detail(cx));
        };
        let history_len = self.state.review_history(&pr).len();
        v_flex()
            .flex_1()
            .min_w_0()
            .h_full()
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
                            .max_w_128()
                            .mx_auto()
                            .p_6()
                            .gap_6()
                            .child(self.description(&pr, cx))
                            .child(self.review(&pr, cx)),
                    ),
            )
    }

    fn empty_detail(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .justify_center()
            .items_center()
            .gap_3()
            .p_6()
            .text_color(cx.theme().muted_foreground)
            .child(Icon::new(IconName::Inbox).large())
            .when(self.loading, |this| {
                this.child(Spinner::new().large())
                    .child("Loading review requests…")
            })
            .when(
                !self.loading && !self.state.filter.trim().is_empty(),
                |this| {
                    this.child("No repository matches this filter.").child(
                        Button::new("clear-filter-empty")
                            .label("Clear filter")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.state.clear_filter();
                                this.reset_selection_view(cx);
                            })),
                    )
                },
            )
            .when(
                !self.loading && self.state.filter.trim().is_empty(),
                |this| {
                    this.child("No review request is selected.").child(
                        Button::new("refresh-empty")
                            .label("Refresh")
                            .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
                    )
                },
            )
    }

    fn detail_header(
        &mut self,
        pr: &PullRequest,
        history_len: usize,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let running = self.state.active.contains(&pr.url);
        let shown = self.state.shown_status(pr).cloned();
        let busy = self.posting.is_some();
        let mut header = v_flex().gap_3();

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
                        .text_xl()
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
                .child(format!(
                    "by {} · updated {}",
                    author_name(pr),
                    pr.updated_at
                )),
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
                        .when(!running, |this| this.primary())
                        .loading(running)
                        .disabled(running)
                        .on_click(cx.listener(|this, _, _, cx| this.run_review(cx))),
                )
                .when(
                    !running && matches!(shown, Some(ReviewStatus::Done(_))),
                    |this| {
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
                    },
                ),
        );

        v_flex()
            .flex_shrink_0()
            .p_6()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(v_flex().w_full().max_w_128().mx_auto().child(header))
    }

    fn description(&mut self, pr: &PullRequest, cx: &Context<Self>) -> impl IntoElement {
        v_flex()
            .gap_2()
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
                            .child("Not reviewed yet. Run the review council to get a verdict."),
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
            Some(ReviewStatus::Done(report)) => {
                section = section.child(self.report(&report, pr, cx));
            }
        }
        section
    }

    fn report(
        &mut self,
        report: &ReviewReport,
        pr: &PullRequest,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let history_len = self.state.review_history(pr).len();
        let offset = self.state.review_offset;
        let index = history_len.saturating_sub(1 + offset);
        let findings = report.confirmed.len() + report.accepted.len();

        v_flex()
            .gap_4()
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(verdict_tag(report.verdict).child(verdict_label(report.verdict)))
                    .when(history_len > 1, |this| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(format!("Review {} of {}", index + 1, history_len)),
                        )
                    })
                    .child(div().flex_1())
                    .when(history_len > 1, |this| {
                        this.child(
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
            .when(!report.confirmed.is_empty(), |this| {
                this.child(findings_section(
                    format!("Confirmed by both reviewers ({})", report.confirmed.len()),
                    &report.confirmed,
                    "reported by both reviewers",
                    cx,
                ))
            })
            .when(!report.accepted.is_empty(), |this| {
                this.child(findings_section(
                    format!("Accepted by the judge ({})", report.accepted.len()),
                    &report.accepted,
                    "accepted by the judge",
                    cx,
                ))
            })
            .when(findings == 0, |this| {
                this.child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().success)
                        .child("No actionable findings were reported."),
                )
            })
            .when(!report.dismissed.is_empty(), |this| {
                this.child(
                    v_flex()
                        .gap_2()
                        .child(section_title(
                            format!("Dismissed as not relevant ({})", report.dismissed.len()),
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
        StatusBar::new()
            .flex_shrink_0()
            .border_t_1()
            .border_color(cx.theme().border)
            .text_xs()
            .left(hints)
            .right(self.notice_element(cx))
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
            .child(self.toolbar(cx))
            .child(
                h_flex()
                    .items_stretch()
                    .flex_1()
                    .min_h_0()
                    .when(self.sidebar_visible, |this| this.child(self.sidebar(cx)))
                    .child(self.detail(cx)),
            )
            .child(self.status_bar(cx))
    }
}

fn section_title(text: impl Into<SharedString>, cx: &App) -> impl IntoElement {
    div()
        .text_xs()
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(cx.theme().muted_foreground)
        .child(text.into())
}

fn findings_section(
    title: String,
    findings: &[Finding],
    source: &str,
    cx: &App,
) -> impl IntoElement {
    v_flex().gap_2().child(section_title(title, cx)).children(
        findings
            .iter()
            .map(|finding| finding_card(finding, source, cx)),
    )
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
        .p_3()
        .rounded_lg()
        .border_1()
        .border_color(cx.theme().border)
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
            this.child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(finding.detail.trim().to_string()),
            )
        })
        .child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(source.to_string()),
        )
}

fn verdict_tag(verdict: Verdict) -> Tag {
    match verdict {
        Verdict::Merge => Tag::success(),
        Verdict::RequestChanges => Tag::danger(),
    }
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

            window.press("secondary-b", cx);
            assert!(!view.read(cx).sidebar_visible);
            window.press("secondary-b", cx);
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
            window.press("secondary-f", cx);
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
