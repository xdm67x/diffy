use crate::api::{self, ReviewAction};
use crate::ask::{self, AskEvent, AskSide, AskStatus, AskTask};
use crate::log;
use crate::models::{Finding, PullRequest, ReviewReport, Verdict};
use crate::review::ReviewEvent;
use crate::state::{ReviewStatus, Row, State};
use crate::store;
use crate::time;

use gpui_kit::base::InteractiveElementExt as _;
use gpui_kit::component::{
    ActiveTheme, Disableable, Icon, IconName, IndexPath, Sizable, WindowExt,
    button::{Button, ButtonVariant, ButtonVariants},
    diff::{
        Diff, DiffAnnotation, DiffFile, DiffHoverHighlight, DiffLinePosition, DiffSide, DiffState,
    },
    h_flex,
    input::{Escape as InputEscape, Input, InputEvent, InputState, Textarea, TextareaState},
    link::Link,
    list::ListItem,
    resizable::{HandleEdge, resize_handle},
    scroll::ScrollableElement,
    searchable_list::SearchableVec,
    select::{Select, SelectState},
    spinner::Spinner,
    status_bar::StatusBar,
    tag::Tag,
    text::TextView,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::cell::Cell;
use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

const CONTEXT: &str = "Diffy";
/// While the filter has focus, letter keys belong to the text field, so this
/// context only keeps escape and the modifier shortcut for the sidebar.
const FILTER_CONTEXT: &str = "DiffyFilter";
/// The sliding diff panel owns this context while it has focus, so ⌘2 closes
/// it again and Esc (bound in the sheet context) dismisses it.
const PANEL_CONTEXT: &str = "DiffPanel";

/// Panel widths the window opens with, and the bounds each panel's resize
/// handle clamps the dragged width to.
const REPOS_DEFAULT_WIDTH: Pixels = px(200.);
const REPOS_MIN_WIDTH: Pixels = px(160.);
const REPOS_MAX_WIDTH: Pixels = px(320.);
const PRS_DEFAULT_WIDTH: Pixels = px(240.);
const PRS_MIN_WIDTH: Pixels = px(208.);
const PRS_MAX_WIDTH: Pixels = px(480.);

/// Clamps a dragged panel width to its usable range.
fn clamp_repos_width(width: Pixels) -> Pixels {
    width.max(REPOS_MIN_WIDTH).min(REPOS_MAX_WIDTH)
}

fn clamp_prs_width(width: Pixels) -> Pixels {
    width.max(PRS_MIN_WIDTH).min(PRS_MAX_WIDTH)
}

/// Which panel a resize handle belongs to.
#[derive(Clone, Copy, PartialEq, Eq)]
enum PanelKind {
    Repos,
    Prs,
}

/// The payload a panel's resize handle drags with. The width itself is
/// recomputed from the pointer position on every move, so the payload only
/// says which panel the drag resizes.
#[derive(Clone)]
struct PanelResize(PanelKind);

impl Render for PanelResize {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        Empty
    }
}

actions!(
    diffy,
    [RefreshPrs, FocusFilter, LeaveFilter, ToggleDiffPanel]
);

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-f", FocusFilter, Some(CONTEXT)),
        KeyBinding::new("cmd-r", RefreshPrs, Some(CONTEXT)),
        KeyBinding::new("cmd-2", ToggleDiffPanel, Some(CONTEXT)),
        KeyBinding::new("escape", LeaveFilter, Some(FILTER_CONTEXT)),
        KeyBinding::new("cmd-r", RefreshPrs, Some(FILTER_CONTEXT)),
        KeyBinding::new("cmd-2", ToggleDiffPanel, Some(FILTER_CONTEXT)),
        KeyBinding::new("cmd-2", ToggleDiffPanel, Some(PANEL_CONTEXT)),
        KeyBinding::new("cmd-2", ToggleDiffPanel, Some("Diff")),
    ]);
}

/// Events arriving from the background threads: reviews and read-only asks.
enum Incoming {
    Review(ReviewEvent),
    Ask(AskEvent),
}

/// What the diff panel knows about the selected PR's patch.
enum DiffView {
    /// The request the panel is waiting for, so a fetch that a refresh or a
    /// retry replaced cannot install an older patch.
    Loading(u64),
    Ready {
        state: Entity<DiffState>,
        files: usize,
        /// The raw patch, kept so a line's code can be quoted in an ask.
        patch: String,
    },
    Failed(String),
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
    list_scroll: ScrollHandle,
    detail_scroll: ScrollHandle,
    loading: bool,
    posting: Option<ReviewAction>,
    notice: Option<Notice>,
    /// Whether the read-only diff panel slides in from the right.
    diff_panel_open: bool,
    /// Width of the repositories panel; the handle on its right edge drags
    /// it between `REPOS_MIN_WIDTH` and `REPOS_MAX_WIDTH`.
    repos_panel_width: Pixels,
    /// Width of the PR list panel; the handle on its right edge drags it
    /// between `PRS_MIN_WIDTH` and `PRS_MAX_WIDTH`.
    prs_panel_width: Pixels,
    /// Set while a panel's resize handle is being dragged, so the widths are
    /// persisted once when the drag ends instead of on every move.
    panel_dragging: bool,
    /// The diff line under the pointer in the panel. Sticky, so the "Ask to
    /// agent" button stays reachable when the pointer moves onto it.
    hovered: Option<DiffLinePosition>,
    /// Fetched patches, keyed by PR url and dropped on every refresh.
    diffs: HashMap<String, DiffView>,
    /// Counter behind the identity of each diff request.
    diff_requests: u64,
    /// "Ask to agent" tasks, keyed by PR url and dropped on every refresh with
    /// the diffs they refer to.
    asks: HashMap<String, Vec<AskTask>>,
    /// Counter behind the identity of each ask task.
    ask_ids: u64,
    /// When each running review started, keyed by PR url.
    started: HashMap<String, Instant>,
    events: Sender<Incoming>,
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
        // Review and ask threads report through a channel; drain it on the
        // foreground thread so `State` is only touched from the UI thread.
        cx.spawn(async move |this, cx| {
            let receiver: Receiver<Incoming> = receiver;
            loop {
                let mut batch = Vec::new();
                while let Ok(event) = receiver.try_recv() {
                    batch.push(event);
                }
                if !batch.is_empty()
                    && this
                        .update(cx, |this, cx| {
                            for event in batch {
                                this.apply_incoming(event, cx);
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
            list_scroll: ScrollHandle::new(),
            detail_scroll: ScrollHandle::new(),
            loading: false,
            posting: None,
            notice: None,
            diff_panel_open: false,
            repos_panel_width: REPOS_DEFAULT_WIDTH,
            prs_panel_width: PRS_DEFAULT_WIDTH,
            panel_dragging: false,
            hovered: None,
            diffs: HashMap::new(),
            diff_requests: 0,
            asks: HashMap::new(),
            ask_ids: 0,
            started: HashMap::new(),
            events,
            _subscriptions: vec![subscription],
        };

        match store::load_reviews() {
            Ok(saved) => {
                log::info(format!("loaded saved reviews for {} PRs", saved.len()));
                this.state
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
                    }))
            }
            Err(error) => this.set_error(format!("Could not load saved reviews: {error:#}")),
        }
        match store::settings() {
            Ok(settings) => {
                this.repos_panel_width = settings
                    .panels
                    .repos
                    .map(|width| clamp_repos_width(px(width as f32)))
                    .unwrap_or(REPOS_DEFAULT_WIDTH);
                this.prs_panel_width = settings
                    .panels
                    .prs
                    .map(|width| clamp_prs_width(px(width as f32)))
                    .unwrap_or(PRS_DEFAULT_WIDTH);
            }
            Err(error) => this.set_error(format!("Could not load settings: {error:#}")),
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
        log::info("refreshing pull requests");
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx.background_spawn(async { api::get_prs() }).await;
            let _ = this.update(cx, |this, cx| {
                this.loading = false;
                match result {
                    Ok(prs) => {
                        log::info(format!("loaded {} pull requests", prs.len()));
                        // A PR that moved may have a new patch, so nothing
                        // fetched earlier is reused after a refresh.
                        this.diffs.clear();
                        this.asks.clear();
                        this.hovered = None;
                        this.state.set_prs(prs);
                        this.notice = Some(Notice {
                            text: "Updated just now".to_string(),
                            tone: Tone::Info,
                        });
                        this.reset_selection_view(cx);
                    }
                    Err(error) => {
                        this.set_error(format!("Could not load pull requests: {error:#}"))
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
        log::info(format!("review started for {target}"));
        self.reset_detail_scroll();
        self.started.insert(pr.url.clone(), Instant::now());
        self.notice = Some(Notice {
            text: format!("Reviewing {target}…"),
            tone: Tone::Info,
        });
        cx.notify();

        let events = self.events.clone();
        crate::review::spawn(pr, move |event| {
            let _ = events.send(Incoming::Review(event));
        });
    }

    fn apply_incoming(&mut self, event: Incoming, cx: &mut Context<Self>) {
        match event {
            Incoming::Review(event) => self.apply(event, cx),
            Incoming::Ask(event) => self.apply_ask(event, cx),
        }
    }

    fn apply(&mut self, event: ReviewEvent, cx: &mut Context<Self>) {
        match &event {
            ReviewEvent::Stage { url, stage } => {
                log::info(format!("{url}: {}", stage.label()));
            }
            ReviewEvent::Progress { .. } => {}
            ReviewEvent::Done { url, report } => {
                log::info(format!(
                    "{url}: review done ({:?}, {} confirmed, {} accepted, {} dismissed)",
                    report.verdict,
                    report.confirmed.len(),
                    report.accepted.len(),
                    report.dismissed.len()
                ));
            }
            ReviewEvent::Failed { url, error } => {
                log::error(format!("{url}: review failed: {error}"));
            }
        }
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
                log::info(format!("saved the review of {url}"));
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

    /// Everything that follows a selection change, including the patch of the
    /// PR the diff panel now shows when it is open.
    fn reset_selection_view(&mut self, cx: &mut Context<Self>) {
        self.reset_detail_scroll();
        self.hovered = None;
        if let Some(row) = self.state.selected_row() {
            self.list_scroll.scroll_to_item(row);
        }
        if self.diff_panel_open {
            self.load_diff(cx);
        }
        cx.notify();
    }

    fn on_toggle_diff_panel(
        &mut self,
        _: &ToggleDiffPanel,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.toggle_diff_panel(window, cx);
    }

    /// Writes the panel widths to `settings.json` so the next launch opens
    /// the window as it was left.
    fn persist_panels(&self) {
        let result = store::save_panels(
            f32::from(self.repos_panel_width) as u32,
            f32::from(self.prs_panel_width) as u32,
        );
        if let Err(error) = result {
            log::error(format!("could not save panel settings: {error:#}"));
        }
    }

    /// Follows a resize handle's drag: the repositories panel starts at the
    /// window's left edge and the PR list right after it, so the pointer's x
    /// inside the shell lands on the seam being dragged.
    fn on_panel_drag_move(
        &mut self,
        event: &DragMoveEvent<PanelResize>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let x = event.event.position.x - event.bounds.origin.x;
        match event.drag(cx).0 {
            PanelKind::Repos => self.repos_panel_width = clamp_repos_width(x),
            PanelKind::Prs => self.prs_panel_width = clamp_prs_width(x - self.repos_panel_width),
        }
        cx.notify();
    }

    /// Persists the dragged widths once the drag ends, not on every move.
    fn on_any_mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.panel_dragging {
            self.panel_dragging = false;
            self.persist_panels();
            cx.notify();
        }
    }

    /// The settings dialog: color theme and the template of the review
    /// comment posted to GitHub. Saving applies the theme immediately.
    fn open_settings_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let settings = store::settings().unwrap_or_default();
        let theme_index = match settings.theme {
            store::ThemePref::Light => 0,
            store::ThemePref::Dark => 1,
        };
        let appearance = cx.new(|cx| {
            SelectState::new(
                SearchableVec::new(vec!["Light", "Dark"]),
                Some(IndexPath::new(theme_index)),
                window,
                cx,
            )
        });
        let template = cx.new(|cx| {
            let mut state = TextareaState::new(window, cx);
            if let Some(template) = &settings.comment_template {
                state.set_value(template.clone(), window, cx);
            }
            state.set_auto_grow(6, 20, cx);
            state
        });
        window.open_alert_dialog(cx, move |dialog, _, _| {
            let content_appearance = appearance.clone();
            let content_template = template.clone();
            let ok_appearance = appearance.clone();
            let ok_template = template.clone();
            dialog
                .title("Settings")
                .content(move |content, _, cx| {
                    content.child(
                        v_flex()
                            .gap_4()
                            .p_4()
                            .w(px(420.))
                            .child(
                                v_flex()
                                    .gap_2()
                                    .child(
                                        div()
                                            .text_sm()
                                            .font_weight(FontWeight::MEDIUM)
                                            .child("Appearance"),
                                    )
                                    .child(
                                        Select::new(&content_appearance)
                                            .w_full()
                                            .accessibility_label("Color theme"),
                                    ),
                            )
                            .child(
                                v_flex()
                                    .gap_2()
                                    .child(
                                        div()
                                            .text_sm()
                                            .font_weight(FontWeight::MEDIUM)
                                            .child("GitHub comment template"),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(
                                                "Placeholders: {pr} {url} {verdict} {summary} {intent} {findings} {dismissed}. Leave empty for the default review format.",
                                            ),
                                    )
                                    .child(
                                        Textarea::new(&content_template)
                                            .aria_label("GitHub comment template")
                                            .min_h(px(140.)),
                                    ),
                            ),
                    )
                })
                .confirm()
                .ok_text("Save")
                .cancel_text("Cancel")
                .on_ok(move |_, _, cx| {
                    let pref = match ok_appearance.read(cx).selected_value() {
                        Some(&"Dark") => store::ThemePref::Dark,
                        _ => store::ThemePref::Light,
                    };
                    let text = ok_template.read(cx).value().to_string();
                    let template = (!text.trim().is_empty()).then_some(text);
                    let _ = store::save_theme(pref);
                    let _ = store::save_comment_template(template);
                    crate::theme::apply(pref, cx);
                    true
                })
        });
    }

    /// One command behind the header button and ⌘2: the panel slides in from
    /// the right when closed, and slides away when open.
    fn toggle_diff_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if window.has_active_sheet(cx) {
            self.close_diff_panel(window, cx);
        } else {
            if self.state.selected_pr().is_none() {
                self.set_error("Select a pull request first.");
                cx.notify();
                return;
            }
            self.open_diff_panel(window, cx);
        }
    }

    fn close_diff_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.diff_panel_open = false;
        self.hovered = None;
        window.close_sheet(cx);
        cx.notify();
    }

    /// Opens the selected PR's diff in a sliding right panel, read-only. The
    /// sheet builder re-runs whenever the window redraws and reads the view
    /// through a weak handle, so the panel always shows the current PR, patch,
    /// and ask tasks.
    fn open_diff_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.diff_panel_open {
            return;
        }
        self.diff_panel_open = true;
        self.load_diff(cx);
        let weak = cx.entity().downgrade();
        let close_weak = weak.clone();
        let panel_focus = cx.focus_handle();
        let initial_focus = Cell::new(true);
        window.open_sheet(cx, move |sheet, window, cx| {
            let Some(this) = weak.upgrade() else {
                return sheet;
            };
            // Focus the panel body once per opening, so ⌘2 and Esc work while
            // the panel is open; later rebuilds must not steal the focus.
            if initial_focus.replace(false) {
                window.focus(&panel_focus, cx);
            }
            let title = this.read(cx).panel_title(cx);
            let body = this.read(cx).panel_body(&weak, &panel_focus, cx);
            let close_weak = close_weak.clone();
            sheet
                .title(title)
                .size(DefiniteLength::Fraction(0.62))
                .p_0()
                // No dimming overlay: the panel slides in beside the review,
                // which stays clickable, so selecting another PR swaps the
                // patch the panel shows.
                .overlay(false)
                .on_close(move |_, _, cx| {
                    let _ = close_weak.update(cx, |this, cx| {
                        this.diff_panel_open = false;
                        this.hovered = None;
                        cx.notify();
                    });
                })
                .child(body)
        });
        cx.notify();
    }

    /// The sheet title: what the panel shows, that it is read-only, and how
    /// many files the patch has.
    fn panel_title(&self, cx: &App) -> impl IntoElement {
        let files = self
            .state
            .selected_pr()
            .and_then(|pr| match self.diffs.get(&pr.url) {
                Some(DiffView::Ready { files, .. }) => {
                    Some(format!("{files} file{}", plural(*files)))
                }
                _ => None,
            });
        h_flex()
            .gap_2()
            .items_center()
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Diff"),
            )
            .child(Tag::new().small().child("Read-only"))
            .child(div().flex_1())
            .when_some(files, |this, files| {
                this.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(files),
                )
            })
    }

    /// The sheet body: the PR identity and a hint, then the diff viewer with
    /// its per-line ask affordance and the task cards. The body owns the panel
    /// focus so the ⌘2 binding keeps working while the panel is open.
    fn panel_body(
        &self,
        weak: &WeakEntity<Diffy>,
        panel_focus: &FocusHandle,
        cx: &App,
    ) -> AnyElement {
        let Some(pr) = self.state.selected_pr().cloned() else {
            return v_flex().size_full().into_any_element();
        };
        let target = format!("{}#{}", pr.repo_name(), pr.number);
        let body = match self.diffs.get(&pr.url) {
            Some(DiffView::Ready { state, .. }) => self.diff_ready(&pr, state.clone(), weak, cx),
            Some(DiffView::Failed(error)) => self.diff_failed(error.clone(), weak, cx),
            Some(DiffView::Loading(_)) | None => v_flex()
                .flex_1()
                .min_h_0()
                .justify_center()
                .items_center()
                .gap_3()
                .child(Spinner::new().large())
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child("Loading the diff…"),
                )
                .into_any_element(),
        };
        let action_weak = weak.clone();
        let ask_weak = weak.clone();
        let ask_target = self.hovered.clone();
        v_flex()
            .id("diff-panel")
            .track_focus(panel_focus)
            .key_context(PANEL_CONTEXT)
            .on_action(move |_: &ToggleDiffPanel, window, cx| {
                let weak = action_weak.clone();
                let _ = weak.update(cx, |this, cx| this.close_diff_panel(window, cx));
            })
            .size_full()
            .child(
                v_flex()
                    .flex_shrink_0()
                    .px_4()
                    .pt_3()
                    .pb_2()
                    .gap_1()
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(
                                div()
                                    .text_xs()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(target),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(pr.title.clone()),
                            )
                            // The keyboard-reachable ask trigger; it acts on the
                            // hovered line and costs no per-line space.
                            .child(
                                Button::new("ask-hovered-line")
                                    .ghost()
                                    .small()
                                    .icon(IconName::Bot)
                                    .label("Ask to agent…")
                                    .tooltip(match &ask_target {
                                        Some(pos) => format!(
                                            "Ask Claude about {}:{} in a read-only session",
                                            pos.path(),
                                            pos.line()
                                        ),
                                        None => "Hover a diff line first".to_string(),
                                    })
                                    .disabled(ask_target.is_none())
                                    .on_click(move |_, window, cx| {
                                        let weak = ask_weak.clone();
                                        let _ = weak.update(cx, |this, cx| {
                                            if let Some(pos) = this.hovered.clone() {
                                                this.open_ask_dialog(pos, window, cx);
                                            }
                                        });
                                    }),
                            ),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("Hover a line and click + to ask about it. The session is read-only: it cannot touch this PR."),
                    ),
            )
            .child(body)
            .into_any_element()
    }

    /// The read-only, syntax-highlighted diff inside the panel, with the ask
    /// button under the hovered line and a card under every line with tasks.
    fn diff_ready(
        &self,
        pr: &PullRequest,
        state: Entity<DiffState>,
        weak: &WeakEntity<Diffy>,
        _cx: &App,
    ) -> AnyElement {
        let annotations = self.ask_annotations(pr);
        let hover_weak = weak.clone();
        let annot_weak = weak.clone();
        let add_weak = weak.clone();
        div()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .child(
                Diff::new(&state)
                    .syntax_highlight(true)
                    .hover_highlight(DiffHoverHighlight::Both)
                    .annotations(annotations)
                    .render_annotation(move |annotation, _, cx| {
                        render_panel_annotation(&annot_weak, annotation, cx)
                    })
                    // The per-line ask trigger lives inside the line row (the
                    // "+" on hover), so it costs no extra vertical space.
                    .on_add_annotation(move |range, window, cx| {
                        let position = DiffLinePosition::new(
                            range.path().clone(),
                            range.side(),
                            range.start(),
                        );
                        let weak = add_weak.clone();
                        let _ =
                            weak.update(cx, |this, cx| this.open_ask_dialog(position, window, cx));
                    })
                    .on_line_hover(move |pos, _, cx| {
                        // Sticky: ignore the pointer leaving a line, so the
                        // header's ask button keeps its target.
                        if let Some(pos) = pos {
                            let pos = pos.clone();
                            let weak = hover_weak.clone();
                            let _ = weak.update(cx, |this, cx| {
                                if this.hovered.as_ref() != Some(&pos) {
                                    this.hovered = Some(pos);
                                    cx.notify();
                                }
                            });
                        }
                    }),
            )
            .into_any_element()
    }

    /// The annotations the diff viewer shows: one card per task on its line.
    fn ask_annotations(&self, pr: &PullRequest) -> Vec<DiffAnnotation> {
        let mut annotations = Vec::new();
        if let Some(tasks) = self.asks.get(&pr.url) {
            for task in tasks {
                annotations.push(DiffAnnotation::line(
                    ElementId::Name(format!("task-{}", task.id).into()),
                    DiffLinePosition::new(task.path.clone(), diff_side(task.side), task.line),
                ));
            }
        }
        annotations
    }

    /// The code of one line of the shown patch, for quoting in an ask.
    fn line_code(&self, url: &str, position: &DiffLinePosition) -> Option<String> {
        let patch = match self.diffs.get(url)? {
            DiffView::Ready { patch, .. } => patch,
            _ => return None,
        };
        ask::line_code(
            patch,
            position.path(),
            ask_side(position.side()),
            position.line(),
        )
    }

    /// The question dialog for one diff line. It only collects the question;
    /// the read-only session is spawned when the dialog is confirmed.
    fn open_ask_dialog(
        &mut self,
        position: DiffLinePosition,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(pr) = self.state.selected_pr().cloned() else {
            return;
        };
        let code = self.line_code(&pr.url, &position);
        let location = format!("{}:{}", position.path(), position.line());
        let editor = cx.new(|cx| {
            let mut state = TextareaState::new(window, cx);
            state.set_placeholder("Ask about this line…", window, cx);
            state.set_auto_grow(3, 10, cx);
            state
        });
        let this = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |dialog, _, _| {
            let this = this.clone();
            let pr = pr.clone();
            let position = position.clone();
            let code = code.clone();
            let editor = editor.clone();
            let location = location.clone();
            dialog
                .title(format!("Ask about {location}"))
                .content({
                    let editor = editor.clone();
                    let pr = pr.clone();
                    let code = code.clone();
                    move |content, _, cx| {
                        let target = format!("{}#{}", pr.repo_name(), pr.number);
                        content.child(
                            v_flex()
                                .gap_2()
                                .p_4()
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(format!(
                                            "Claude answers in a read-only session: it cannot run commands, and nothing is posted to {target}."
                                        )),
                                )
                                .child(
                                    div()
                                        .p_2()
                                        .rounded_md()
                                        .bg(cx.theme().muted)
                                        .font_family(cx.theme().mono_font_family.clone())
                                        .text_xs()
                                        .child(match &code {
                                            Some(code) => code.trim_end().to_string(),
                                            None => {
                                                "(the line is not part of the visible patch)"
                                                    .to_string()
                                            }
                                        }),
                                )
                                .child(
                                    Textarea::new(&editor)
                                        .aria_label("Your question")
                                        .min_h(px(72.)),
                                ),
                        )
                    }
                })
                .confirm()
                .ok_text("Ask")
                .cancel_text("Cancel")
                .on_ok(move |_, _, cx| {
                    let question = editor.read(cx).value().to_string();
                    let this = this.clone();
                    let pr = pr.clone();
                    let position = position.clone();
                    let code = code.clone();
                    let _ = this.update(cx, |this, cx| {
                        this.submit_ask(pr, position, code, question, cx)
                    });
                    true
                })
        });
    }

    /// Creates the task for a line question and starts the read-only session.
    /// The task shows under the line in the panel while it runs and after it
    /// finishes; nothing here writes to the PR.
    fn submit_ask(
        &mut self,
        pr: PullRequest,
        position: DiffLinePosition,
        code: Option<String>,
        question: String,
        cx: &mut Context<Self>,
    ) {
        let question = question.trim().to_string();
        if question.is_empty() {
            self.set_error("Type a question about this line first.");
            cx.notify();
            return;
        }
        let Some(patch) = self.diffs.get(&pr.url).and_then(|view| match view {
            DiffView::Ready { patch, .. } => Some(patch.clone()),
            _ => None,
        }) else {
            self.set_error("The diff is still loading; try again in a moment.");
            cx.notify();
            return;
        };
        self.ask_ids += 1;
        let id = self.ask_ids;
        let side = ask_side(position.side());
        let task = AskTask {
            id,
            path: position.path().to_string(),
            side,
            line: position.line(),
            code: code.or_else(|| ask::line_code(&patch, position.path(), side, position.line())),
            question: question.clone(),
            progress: String::new(),
            status: AskStatus::Running,
        };
        log::info(format!(
            "{}: ask #{id} about {}:{}",
            pr.url, task.path, task.line
        ));
        self.asks
            .entry(pr.url.clone())
            .or_default()
            .push(task.clone());
        self.notice = Some(Notice {
            text: format!(
                "Asking Claude about {}:{} (read-only)…",
                task.path, task.line
            ),
            tone: Tone::Info,
        });
        let events = self.events.clone();
        let request = ask::AskRequest {
            pr,
            diff: patch,
            path: task.path.clone(),
            side: task.side,
            line: task.line,
            code: task.code.clone(),
            question,
        };
        ask::spawn(id, request, move |event| {
            let _ = events.send(Incoming::Ask(event));
        });
        cx.notify();
    }

    /// Runs a failed task again with the same line and question.
    fn retry_ask(&mut self, url: &str, id: u64, cx: &mut Context<Self>) {
        let Some(pr) = self.state.prs.iter().find(|pr| pr.url == url).cloned() else {
            return;
        };
        let Some(task) = self
            .asks
            .get(url)
            .and_then(|tasks| tasks.iter().find(|task| task.id == id))
            .cloned()
        else {
            return;
        };
        let Some(patch) = self.diffs.get(url).and_then(|view| match view {
            DiffView::Ready { patch, .. } => Some(patch.clone()),
            _ => None,
        }) else {
            return;
        };
        if let Some(task) = self
            .asks
            .get_mut(url)
            .and_then(|tasks| tasks.iter_mut().find(|task| task.id == id))
        {
            task.status = AskStatus::Running;
            task.progress.clear();
        }
        let events = self.events.clone();
        let request = ask::AskRequest {
            pr,
            diff: patch,
            path: task.path,
            side: task.side,
            line: task.line,
            code: task.code,
            question: task.question,
        };
        ask::spawn(id, request, move |event| {
            let _ = events.send(Incoming::Ask(event));
        });
        cx.notify();
    }

    fn ask_task_mut(&mut self, url: &str, id: u64) -> Option<&mut AskTask> {
        self.asks
            .get_mut(url)?
            .iter_mut()
            .find(|task| task.id == id)
    }

    /// Applies one ask event to the task it belongs to and to the status bar.
    fn apply_ask(&mut self, event: AskEvent, cx: &mut Context<Self>) {
        match &event {
            AskEvent::Progress { url, id, text } => {
                log::info(format!("{url}: ask #{id} · {text}"));
            }
            AskEvent::Done { url, id, answer } => {
                log::info(format!(
                    "{url}: ask #{id} answered ({} chars)",
                    answer.len()
                ));
            }
            AskEvent::Failed { url, id, error } => {
                log::error(format!("{url}: ask #{id} failed: {error}"));
            }
        }
        match event {
            AskEvent::Progress { url, id, text } => {
                if let Some(task) = self.ask_task_mut(&url, id) {
                    task.progress = text;
                }
            }
            AskEvent::Done { url, id, answer } => {
                if let Some(task) = self.ask_task_mut(&url, id) {
                    task.status = AskStatus::Done(answer);
                }
                self.notice = Some(Notice {
                    text: format!("Answered the question on {}.", self.state.label(&url)),
                    tone: Tone::Success,
                });
            }
            AskEvent::Failed { url, id, error } => {
                if let Some(task) = self.ask_task_mut(&url, id) {
                    task.status = AskStatus::Failed(error.clone());
                }
                self.set_error(format!(
                    "The ask on {} failed: {error}",
                    self.state.label(&url)
                ));
            }
        }
        cx.notify();
    }

    /// Fetches the patch the first time the diff panel shows this PR; the
    /// result is kept by PR url until the next refresh.
    fn load_diff(&mut self, cx: &mut Context<Self>) {
        let Some(pr) = self.state.selected_pr().cloned() else {
            return;
        };
        if self.diffs.contains_key(&pr.url) {
            return;
        }
        self.diff_requests += 1;
        let request = self.diff_requests;
        self.diffs
            .insert(pr.url.clone(), DiffView::Loading(request));
        cx.notify();
        let url = pr.url.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move { api::fetch_diff(&pr) })
                .await;
            let _ = this.update(cx, |this, cx| {
                if !this.diff_is_pending(&url, request) {
                    return;
                }
                match result {
                    Ok(patch) => {
                        let parsed = DiffFile::parse(&patch).map_err(|error| error.to_string());
                        this.set_diff(&url, patch, parsed, cx);
                    }
                    Err(error) => this.set_diff(&url, String::new(), Err(format!("{error:#}")), cx),
                }
            });
        })
        .detach();
    }

    /// True while the panel still waits for this fetch. A refresh or a retry
    /// replaces the request, and the older patch must not install over it.
    fn diff_is_pending(&self, url: &str, request: u64) -> bool {
        matches!(self.diffs.get(url), Some(DiffView::Loading(current)) if *current == request)
    }

    /// Installs a fetched patch. An empty patch reads like a failure: there is
    /// nothing to scroll either way, and both offer another attempt.
    fn set_diff(
        &mut self,
        url: &str,
        patch: String,
        parsed: Result<Vec<DiffFile>, String>,
        cx: &mut Context<Self>,
    ) {
        let view = match parsed {
            Ok(changes) if changes.is_empty() => {
                DiffView::Failed("This pull request has no file changes to show.".to_string())
            }
            Ok(changes) => {
                let files = changes.len();
                DiffView::Ready {
                    state: cx.new(|cx| DiffState::new(changes, cx)),
                    files,
                    patch,
                }
            }
            Err(error) => {
                log::error(format!("could not load the diff of {url}: {error}"));
                DiffView::Failed(error)
            }
        };
        self.diffs.insert(url.to_string(), view);
        cx.notify();
    }

    fn retry_diff(&mut self, cx: &mut Context<Self>) {
        if let Some(url) = self.state.selected_pr().map(|pr| pr.url.clone()) {
            self.diffs.remove(&url);
        }
        self.load_diff(cx);
    }

    fn reset_detail_scroll(&mut self) {
        self.detail_scroll.set_offset(point(px(0.), px(0.)));
    }

    fn repo_count(&self) -> usize {
        self.state.groups.len()
    }

    fn pr_count(&self) -> usize {
        self.state.groups.iter().map(|group| group.prs.len()).sum()
    }

    fn set_error(&mut self, text: impl Into<String>) {
        let text = text.into();
        log::error(&text);
        self.notice = Some(Notice {
            text,
            tone: Tone::Error,
        });
    }

    /// Opens the confirmation for posting the shown review. The body is
    /// editable there, and nothing reaches GitHub before the dialog's action
    /// button is pressed.
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
        // A configured template renders the placeholders; without one the
        // report's built-in markdown is the body.
        let body = match store::settings()
            .ok()
            .and_then(|settings| settings.comment_template)
            .filter(|template| !template.trim().is_empty())
        {
            Some(template) => crate::models::render_comment_template(&template, &pr, &report),
            None => report.to_markdown(),
        };
        let target = format!("{}#{}", pr.repo_name(), pr.number);
        let editor = cx.new(|cx| {
            let mut state = TextareaState::new(window, cx);
            state.set_value(body, window, cx);
            state.set_auto_grow(8, 24, cx);
            state
        });
        let this = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |dialog, _, _| {
            let this = this.clone();
            let pr = pr.clone();
            let report = report.clone();
            let editor = editor.clone();
            dialog
                .title(confirm_title(action))
                .content({
                    let editor = editor.clone();
                    let target = target.clone();
                    move |content, _, cx| {
                        content.child(
                            v_flex()
                                .gap_2()
                                .p_4()
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(format!(
                                            "Diffy posts this to {target} on GitHub as you, and adds one-line fixes as inline suggestions. Edit the body first if you want."
                                        )),
                                )
                                .child(
                                    Textarea::new(&editor)
                                        .aria_label("Review body")
                                        .min_h(px(240.)),
                                ),
                        )
                    }
                })
                .confirm()
                .ok_text(action_label(action))
                .ok_variant(action_variant(action))
                .cancel_text("Cancel")
                .on_ok(move |_, _, cx| {
                    let this = this.clone();
                    let pr = pr.clone();
                    let report = report.clone();
                    let body = editor.read(cx).value().to_string();
                    let _ =
                        this.update(cx, |this, cx| this.post(pr, action, body, report, cx));
                    true
                })
        });
    }

    fn post(
        &mut self,
        pr: PullRequest,
        action: ReviewAction,
        body: String,
        report: ReviewReport,
        cx: &mut Context<Self>,
    ) {
        if self.posting.is_some() {
            return;
        }
        self.posting = Some(action);
        let target = format!("{}#{}", pr.repo_name(), pr.number);
        log::info(format!("posting {action:?} review to {target}"));
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move { api::submit_review(&pr, action, &body, &report) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.posting = None;
                match result {
                    Ok(()) => {
                        log::info(format!("posted {action:?} review to {target}"));
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

    fn filter_is_focused(&self, window: &Window, cx: &App) -> bool {
        self.filter.focus_handle(cx).is_focused(window)
    }

    fn focus_filter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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

    /// The strip above the content, a drag surface for the window.
    fn main_top_bar(&mut self, _: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .h_9()
            .flex_shrink_0()
            .px_4()
            .items_center()
            .window_control_area(WindowControlArea::Drag)
    }

    /// The left panel: the repositories with their PR counts. Selecting one
    /// shows its PRs in the middle panel.
    fn repo_panel(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut list = v_flex()
            .id("repo-list")
            .flex_1()
            .min_h_0()
            .py_2()
            .overflow_y_scrollbar();
        for group in self.state.groups.clone() {
            list = list.child(self.repo_row(&group.repo, group.prs.len(), cx));
        }

        let repos = self.repo_count();

        v_flex()
            .id("repos-panel")
            .relative()
            .w(self.repos_panel_width)
            .h_full()
            .flex_shrink_0()
            .bg(cx.theme().sidebar)
            .child(
                h_flex()
                    .h_9()
                    .flex_shrink_0()
                    .px_3()
                    .items_center()
                    .window_control_area(WindowControlArea::Drag)
                    // The lights float over the left end, so equal spacers centre
                    // the wordmark in the strip.
                    .child(div().flex_1())
                    .child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(cx.theme().sidebar_foreground)
                            .child("Diffy"),
                    )
                    .child(div().flex_1()),
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
                            .child("Repositories"),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(repos.to_string()),
                    ),
            )
            .child(list)
            .child(
                h_flex()
                    .h_9()
                    .flex_shrink_0()
                    .px_3()
                    .gap_2()
                    .items_center()
                    .border_t_1()
                    .border_color(cx.theme().sidebar_border)
                    .child(
                        Button::new("open-settings")
                            .ghost()
                            .small()
                            .icon(IconName::Settings)
                            .label("Settings")
                            .tooltip("Color theme and comment template")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.open_settings_dialog(window, cx)
                            })),
                    ),
            )
            // The seam between this panel and the PR list is the resize
            // handle's own hairline, so the panel draws no border of its own.
            .child(self.panel_resize_handle(PanelKind::Repos, REPOS_DEFAULT_WIDTH, cx))
    }

    fn repo_row(&self, repo: &str, count: usize, cx: &mut Context<Self>) -> impl IntoElement {
        let selected = self.state.selected_repo.as_deref() == Some(repo);
        let repo_name = repo.to_string();
        ListItem::new(ElementId::Name(format!("repo-{repo}").into()))
            .selected(selected)
            .accessibility_label(format!("{repo}, {count} pull requests"))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.state.select_repo(&repo_name);
                this.reset_selection_view(cx);
            }))
            .child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .items_center()
                    .child(
                        Icon::new(IconName::Github)
                            .small()
                            .text_color(cx.theme().muted_foreground),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_sm()
                            .child(repo.to_string()),
                    )
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(count.to_string()),
                    ),
            )
    }

    /// The middle panel: the PRs of the selected repository, with the filter
    /// on top.
    fn pr_panel(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
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
                Row::Pr(index) => {
                    if let Some(pr) = self.state.prs.get(index).cloned() {
                        list = list.child(self.pr_row(&pr, cx));
                    }
                }
            }
        }

        let visible = self.state.prs.len();
        let repo = self.state.selected_repo.clone().unwrap_or_default();
        let prs = self.pr_count();
        let repos = self.repo_count();
        let active = self.state.active.len();

        v_flex()
            .id("prs-panel")
            .relative()
            .w(self.prs_panel_width)
            .h_full()
            .flex_shrink_0()
            .bg(cx.theme().sidebar)
            .child(
                v_flex().px_3().pt_2().pb_3().child(
                    Input::new(&self.filter)
                        .id("pr-filter")
                        .aria_label("Filter pull requests")
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
                    .pt_1()
                    .pb_1()
                    .gap_2()
                    .items_center()
                    .child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(cx.theme().sidebar_foreground)
                            .truncate()
                            .child(repo),
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
                            .tooltip("Reload pull requests (⌘R)")
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
                                    "{prs} pull request{} · {repos} repo{}",
                                    plural(prs),
                                    plural(repos)
                                )),
                        )
                    }),
            )
            // The seam between this panel and the detail is the resize
            // handle's own hairline.
            .child(self.panel_resize_handle(PanelKind::Prs, PRS_DEFAULT_WIDTH, cx))
    }

    /// The drag band on a panel's right edge: dragging it resizes the panel,
    /// double-clicking restores `default_width`.
    fn panel_resize_handle(
        &mut self,
        kind: PanelKind,
        default_width: Pixels,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let weak = cx.entity().downgrade();
        let id = match kind {
            PanelKind::Repos => "repos-edge",
            PanelKind::Prs => "prs-edge",
        };
        resize_handle(id, Axis::Horizontal)
            .inside(HandleEdge::Trailing)
            .on_drag(PanelResize(kind), move |drag, _, _, cx| {
                if let Some(this) = weak.upgrade() {
                    this.update(cx, |this, _| this.panel_dragging = true);
                }
                cx.new(|_| drag.clone())
            })
            .on_double_click(cx.listener(move |this, _, _, cx| {
                match kind {
                    PanelKind::Repos => this.repos_panel_width = default_width,
                    PanelKind::Prs => this.prs_panel_width = default_width,
                }
                this.persist_panels();
                cx.notify();
            }))
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
            .child(self.review_body(&pr, cx))
    }

    fn review_body(&mut self, pr: &PullRequest, cx: &mut Context<Self>) -> AnyElement {
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
                    .child(self.review(pr, cx))
                    .child(self.description(pr, cx)),
            )
            .into_any_element()
    }

    /// The panel's failure state for a patch that could not be loaded.
    fn diff_failed(&self, error: String, weak: &WeakEntity<Diffy>, cx: &App) -> AnyElement {
        let weak = weak.clone();
        v_flex()
            .flex_1()
            .min_h_0()
            .justify_center()
            .items_center()
            .gap_3()
            .p_6()
            .child(
                Icon::new(IconName::CircleAlert)
                    .large()
                    .text_color(cx.theme().danger),
            )
            .child(
                div()
                    .text_lg()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Could not load the diff"),
            )
            .child(
                div()
                    .max_w_96()
                    .text_sm()
                    .text_center()
                    .text_color(cx.theme().muted_foreground)
                    .child(error),
            )
            .child(
                Button::new("retry-diff")
                    .label("Try again")
                    .outline()
                    .on_click(move |_, _, cx| {
                        let weak = weak.clone();
                        let _ = weak.update(cx, |this, cx| this.retry_diff(cx));
                    }),
            )
            .into_any_element()
    }

    fn empty_detail(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let loading = self.loading;
        let filtered = !self.state.filter.trim().is_empty();
        let (title, body) = if loading {
            (
                "Loading pull requests",
                "Fetching the open PRs waiting on you.",
            )
        } else if filtered {
            (
                "No pull request matches this filter",
                "The filter searches the title, the author, and the number within the selected repository.",
            )
        } else {
            (
                "No pull requests",
                "Open pull requests where you or your team is asked to review, and the open pull requests you wrote, appear here.",
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
                .when(pr.by_me, |this| this.child(Tag::new().child("Yours"))),
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
                .child(
                    Button::new("toggle-diff-panel")
                        .label(if self.diff_panel_open {
                            "Hide diff"
                        } else {
                            "Diff"
                        })
                        .icon(if self.diff_panel_open {
                            IconName::PanelRightClose
                        } else {
                            IconName::PanelRightOpen
                        })
                        .outline()
                        .tooltip("Show the PR diff in a side panel, read-only (⌘2)")
                        .on_click(
                            cx.listener(|this, _, window, cx| this.toggle_diff_panel(window, cx)),
                        ),
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
                        // GitHub refuses an approval or a change request on your
                        // own PR, so only the comment is offered there.
                        .when(!pr.by_me, |this| {
                            this.child(
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
                        })
                        .when(pr.by_me, |this| {
                            this.child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(
                                        "GitHub only accepts comments on your own pull requests.",
                                    ),
                            )
                        })
                }),
        );

        v_flex()
            .flex_shrink_0()
            .px_6()
            .pt_2()
            .pb_6()
            .bg(cx.theme().sidebar)
            .border_b_1()
            .border_color(cx.theme().sidebar_border)
            .child(
                v_flex()
                    .w_full()
                    .max_w(rems(40.))
                    .mx_auto()
                    .gap_3()
                    .child(header),
            )
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
        let hints = "⌘2 diff panel · ⌘F filter · ⌘R refresh";
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
            .on_action(cx.listener(Self::on_refresh))
            .on_action(cx.listener(Self::on_focus_filter))
            .on_action(cx.listener(Self::on_leave_filter))
            .on_action(cx.listener(Self::on_input_escape))
            .on_action(cx.listener(Self::on_toggle_diff_panel))
            // Any mouse up ends a handle drag; the listener persists the
            // dragged width only when one was in progress.
            .capture_any_mouse_up(cx.listener(Self::on_any_mouse_up))
            .child(
                h_flex()
                    .id("shell")
                    .items_stretch()
                    .size_full()
                    .on_drag_move(cx.listener(Self::on_panel_drag_move))
                    .child(self.repo_panel(cx))
                    .child(self.pr_panel(cx))
                    .child(self.main_pane(cx)),
            )
    }
}

fn ask_side(side: DiffSide) -> AskSide {
    match side {
        DiffSide::Original => AskSide::Original,
        DiffSide::Modified => AskSide::Modified,
    }
}

fn diff_side(side: AskSide) -> DiffSide {
    match side {
        AskSide::Original => DiffSide::Original,
        AskSide::Modified => DiffSide::Modified,
    }
}

/// Renders one annotation inside the diff panel: the task card for a
/// `task-{id}` line.
fn render_panel_annotation(
    weak: &WeakEntity<Diffy>,
    annotation: &DiffAnnotation,
    cx: &App,
) -> AnyElement {
    let id = annotation.id().clone();
    let task_id = match &id {
        ElementId::Name(name) => name
            .strip_prefix("task-")
            .and_then(|rest| rest.parse::<u64>().ok()),
        _ => None,
    };
    let Some(task_id) = task_id else {
        return div().into_any_element();
    };
    let Some(this) = weak.upgrade() else {
        return div().into_any_element();
    };
    let this = this.read(cx);
    let Some(url) = this.state.selected.clone() else {
        return div().into_any_element();
    };
    let Some(task) = this
        .asks
        .get(&url)
        .and_then(|tasks| tasks.iter().find(|task| task.id == task_id).cloned())
    else {
        return div().into_any_element();
    };
    ask_task_card(&task, &url, weak, cx)
}

/// One ask task under its diff line, kept compact: a single header row with
/// the question and the line identity, then the running state, a height-capped
/// answer, or the failure with a retry.
fn ask_task_card(task: &AskTask, url: &str, weak: &WeakEntity<Diffy>, cx: &App) -> AnyElement {
    let status = match &task.status {
        AskStatus::Running => h_flex()
            .gap_1p5()
            .items_center()
            .child(Spinner::new().small().color(cx.theme().warning))
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(if task.progress.is_empty() {
                        "Asking Claude (read-only)…".to_string()
                    } else {
                        format!("{} (read-only)", task.progress)
                    }),
            )
            .into_any_element(),
        AskStatus::Done(answer) => div()
            .max_h(px(200.))
            .overflow_y_scrollbar()
            .child(
                TextView::markdown(
                    ElementId::Name(format!("ask-answer-{}", task.id).into()),
                    answer.clone(),
                )
                .selectable(true),
            )
            .into_any_element(),
        AskStatus::Failed(error) => {
            let weak = weak.clone();
            let url = url.to_string();
            let id = task.id;
            h_flex()
                .gap_2()
                .items_center()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_xs()
                        .text_color(cx.theme().danger)
                        .truncate()
                        .child(error.clone()),
                )
                .child(
                    Button::new(ElementId::Name(format!("retry-ask-{id}").into()))
                        .ghost()
                        .small()
                        .icon(IconName::RotateCw)
                        .label("Retry")
                        .on_click(move |_, _, cx| {
                            let weak = weak.clone();
                            let url = url.clone();
                            let _ = weak.update(cx, |this, cx| this.retry_ask(&url, id, cx));
                        }),
                )
                .into_any_element()
        }
    };
    v_flex()
        .gap_1p5()
        .child(
            h_flex()
                .gap_1p5()
                .items_center()
                .child(
                    Icon::new(IconName::Bot)
                        .xsmall()
                        .text_color(cx.theme().muted_foreground),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_xs()
                        .font_weight(FontWeight::MEDIUM)
                        .truncate()
                        .child(task.question.clone()),
                )
                .child(
                    div()
                        .flex_shrink_0()
                        .text_xs()
                        .font_family(cx.theme().mono_font_family.clone())
                        .text_color(cx.theme().muted_foreground)
                        .child(format!(
                            "{}:{} · {}",
                            task.path,
                            task.line,
                            task.side.label()
                        )),
                ),
        )
        .child(status)
        .into_any_element()
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
    use super::{
        DiffView, Diffy, PRS_DEFAULT_WIDTH, PRS_MAX_WIDTH, REPOS_DEFAULT_WIDTH, REPOS_MIN_WIDTH,
        bind_keys,
    };
    use crate::ask::{AskEvent, AskSide, AskStatus, AskTask};
    use crate::models::{Author, PullRequest, Repository};
    use gpui_kit::component::WindowExt as _;
    use gpui_kit::component::diff::{DiffFile, DiffLinePosition, DiffSide};
    use gpui_kit::test::{TestQueryExt, TestWindowExt};
    use gpui_kit::{
        AnyWindowHandle, AppContext as _, Bounds, Entity, Focusable as _, Modifiers, MouseButton,
        Role, TestAppContext, VisualTestContext, WindowBounds, WindowOptions, point, px, size,
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
            by_me: false,
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
    fn typing_a_filter_narrows_the_list_and_escape_returns_to_it(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        cx.update(bind_keys);
        let (handle, view) = open_app(
            cx,
            vec![
                pr("acme/api", 1, "one"),
                pr("acme/api", 2, "two"),
                pr("acme/web", 3, "three"),
            ],
        );
        // The list shows the selected repository's PRs only; pick acme/api's
        // second PR as the selection before filtering.
        cx.update(|cx| {
            view.update(cx, |view, _| {
                view.state.select_repo("acme/api");
                view.state.select_url("https://github.com/acme/api/pull/2");
            });
        });

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

        cx.update_window(handle, |_, window, cx| window.input("one", cx))
            .unwrap();

        // Input events reach the view after the update that produced them.
        cx.update(|cx| {
            assert_eq!(view.read(cx).filter.read(cx).value().to_string(), "one");
            assert_eq!(view.read(cx).state.filter, "one");
            assert_eq!(
                view.read(cx).state.selected_pr().map(|pr| pr.number),
                Some(1),
                "the filter that hides the selection falls back to the visible PR"
            );
            assert!(view.read(cx).state.prs.iter().all(|pr| pr.number == 1));
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
            assert_eq!(view.read(cx).state.filter, "one");
        })
        .unwrap();
    }

    /// The patch comes from `gh` in production; the test seeds one so the panel
    /// can render without a network call.
    const PATCH: &str = "diff --git a/src/main.rs b/src/main.rs
--- a/src/main.rs
+++ b/src/main.rs
@@ -1,2 +1,2 @@
-old line
+new line
 unchanged
";

    const PR_URL: &str = "https://github.com/acme/api/pull/1";

    fn seed_patch(view: &Entity<Diffy>, cx: &mut gpui_kit::App) {
        view.update(cx, |view, cx| {
            let url = view.state.selected_pr().unwrap().url.clone();
            view.set_diff(
                &url,
                PATCH.to_string(),
                Ok(DiffFile::parse(PATCH).expect("patch parses")),
                cx,
            );
        });
    }

    #[gpui_kit::test]
    fn the_diff_shortcut_opens_the_sliding_panel_and_escape_closes_it(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        cx.update(bind_keys);
        let (handle, view) = open_app(cx, vec![pr("acme/api", 1, "one")]);
        cx.update(|cx| seed_patch(&view, cx));

        cx.update_window(handle, |_, window, cx| {
            focus_window(&view, window, cx);
            window.press("cmd-2", cx);
            window.render_frame(cx);
        })
        .unwrap();

        cx.update(|cx| {
            let view = view.read(cx);
            assert!(view.diff_panel_open, "⌘2 did not open the diff panel");
            assert!(
                matches!(
                    view.diffs.get(PR_URL),
                    Some(DiffView::Ready { files: 1, .. })
                ),
                "the parsed patch did not reach the viewer"
            );
        });

        cx.update_window(handle, |_, window, cx| {
            assert!(window.has_active_sheet(cx), "the sliding panel is not open");
            let docs = window.find_all_by_role(Role::Document);
            let pane = docs
                .first()
                .expect("the diff viewer is not in the frame")
                .bounds();
            assert!(
                pane.size.height > px(200.),
                "the diff pane collapsed to {}px",
                f32::from(pane.size.height)
            );
            window.press("escape", cx);
            window.render_frame(cx);
        })
        .unwrap();

        cx.update_window(handle, |_, window, cx| {
            assert!(
                !window.has_active_sheet(cx),
                "escape did not close the panel"
            );
        })
        .unwrap();
        cx.update(|cx| assert!(!view.read(cx).diff_panel_open));
    }

    /// Hovering a line surfaces the read-only "Ask to agent" action under it,
    /// and a finished ask shows its answer as a task card on the same line.
    #[gpui_kit::test]
    fn hovering_a_line_shows_ask_to_agent_and_tasks_render_answers(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        cx.update(bind_keys);
        let (handle, view) = open_app(cx, vec![pr("acme/api", 1, "one")]);
        cx.update(|cx| seed_patch(&view, cx));

        cx.update_window(handle, |_, window, cx| {
            focus_window(&view, window, cx);
            window.press("cmd-2", cx);
            window.render_frame(cx);
        })
        .unwrap();

        // Hover the added line, then finish a task on it.
        cx.update(|cx| {
            view.update(cx, |view, cx| {
                view.hovered = Some(DiffLinePosition::new("src/main.rs", DiffSide::Modified, 2));
                view.asks.insert(
                    PR_URL.to_string(),
                    vec![AskTask {
                        id: 1,
                        path: "src/main.rs".to_string(),
                        side: AskSide::Modified,
                        line: 2,
                        code: Some("+new line".to_string()),
                        question: "why this change?".to_string(),
                        progress: String::new(),
                        status: AskStatus::Running,
                    }],
                );
                view.apply_ask(
                    AskEvent::Done {
                        url: PR_URL.to_string(),
                        id: 1,
                        answer: "because the old line was wrong".to_string(),
                    },
                    cx,
                );
            });
        });

        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            let buttons = window.find_all_by_role(Role::Button);
            let labels: Vec<Option<&str>> = buttons.iter().map(|b| b.label()).collect();
            // The header ask trigger acts on the hovered line and takes no
            // per-line space; the in-row "+" is the pointer path.
            assert!(
                labels.contains(&Some("Ask to agent…")),
                "the ask button is missing from the panel header: {labels:?}"
            );
            assert!(
                labels
                    .iter()
                    .any(|label| label.is_some_and(|l| l.starts_with("Add annotation to line"))),
                "the per-line + ask trigger is missing: {labels:?}"
            );
            let ask = buttons
                .iter()
                .find(|b| b.label() == Some("Ask to agent…"))
                .expect("ask button");
            assert_ne!(ask.disabled(), Some(true), "hovered line should enable ask");
        })
        .unwrap();

        cx.update(|cx| {
            let view = view.read(cx);
            let tasks = &view.asks[PR_URL];
            assert_eq!(tasks.len(), 1);
            assert!(
                matches!(&tasks[0].status, AskStatus::Done(answer) if answer.contains("old line")),
                "the ask event did not land on the task"
            );
            assert_eq!(
                view.ask_annotations(view.state.selected_pr().unwrap())
                    .len(),
                1,
                "only the task card annotates the diff; the ask trigger lives in the line row"
            );
        });
    }

    /// The panel quotes the hovered line's code from the stored patch.
    #[gpui_kit::test]
    fn the_panel_reads_line_code_from_the_patch(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        cx.update(bind_keys);
        let (handle, view) = open_app(cx, vec![pr("acme/api", 1, "one")]);
        cx.update(|cx| seed_patch(&view, cx));

        cx.update(|cx| {
            let view = view.read(cx);
            let added = DiffLinePosition::new("src/main.rs", DiffSide::Modified, 1);
            let removed = DiffLinePosition::new("src/main.rs", DiffSide::Original, 1);
            let context = DiffLinePosition::new("src/main.rs", DiffSide::Modified, 2);
            // The patch marker is stripped: the ask quotes the source line.
            assert_eq!(view.line_code(PR_URL, &added).as_deref(), Some("new line"));
            assert_eq!(
                view.line_code(PR_URL, &removed).as_deref(),
                Some("old line")
            );
            assert_eq!(
                view.line_code(PR_URL, &context).as_deref(),
                Some("unchanged")
            );
        });

        // Opening the panel does not hit the network: the seeded patch is kept.
        cx.update_window(handle, |_, window, cx| {
            focus_window(&view, window, cx);
            window.press("cmd-2", cx);
            window.render_frame(cx);
            assert!(window.has_active_sheet(cx));
        })
        .unwrap();
        cx.update(|cx| {
            assert!(matches!(
                view.read(cx).diffs.get(PR_URL),
                Some(DiffView::Ready { files: 1, .. })
            ));
        });
    }

    /// The resize handles on both panel seams drag the widths within the
    /// clamped bounds, and the drag end persists both.
    #[gpui_kit::test]
    fn the_panel_handles_resize_the_panels_within_the_bounds_and_persist(cx: &mut TestAppContext) {
        // Keep the persisted settings out of the real ~/.diffy.
        let home = std::env::temp_dir().join(format!("diffy-panels-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        unsafe { std::env::set_var("DIFFY_HOME", &home) };

        cx.update(gpui_kit::init);
        cx.update(bind_keys);
        let (handle, view) = open_app(cx, vec![pr("acme/api", 1, "one")]);
        let mut cx = VisualTestContext::from_window(handle, cx);
        let draw = |cx: &mut VisualTestContext| {
            cx.update_window(handle, |_, window, cx| window.render_frame(cx))
                .unwrap();
        };

        cx.update(|_, cx| {
            let view = view.read(cx);
            assert_eq!(view.repos_panel_width, REPOS_DEFAULT_WIDTH);
            assert_eq!(view.prs_panel_width, PRS_DEFAULT_WIDTH);
        });

        // Drag a seam from `from` to `to`; the first move past the drag
        // threshold starts the drag, the next is what the shell hears.
        let drag = |cx: &mut VisualTestContext, from: f32, to: f32| {
            draw(cx);
            cx.simulate_mouse_down(
                point(px(from - 2.), px(300.)),
                MouseButton::Left,
                Modifiers::default(),
            );
            draw(cx);
            cx.simulate_mouse_move(
                point(px(from - 6.), px(300.)),
                Some(MouseButton::Left),
                Modifiers::default(),
            );
            draw(cx);
            cx.simulate_mouse_move(
                point(px(to), px(300.)),
                Some(MouseButton::Left),
                Modifiers::default(),
            );
            draw(cx);
            cx.simulate_mouse_up(
                point(px(to), px(300.)),
                MouseButton::Left,
                Modifiers::default(),
            );
            draw(cx);
        };

        // Repos seam at x=200: drag it to 260.
        drag(&mut cx, 200., 260.);
        cx.update(|_, cx| assert_eq!(view.read(cx).repos_panel_width, px(260.)));

        // PR list seam now at 260+240=500: drag it to 700, so the list width
        // is 700-260=440.
        drag(&mut cx, 500., 700.);
        cx.update(|_, cx| assert_eq!(view.read(cx).prs_panel_width, px(440.)));

        // Past the bounds the drag clamps: PR list to the max, repos to the min.
        drag(&mut cx, 700., 2000.);
        cx.update(|_, cx| assert_eq!(view.read(cx).prs_panel_width, PRS_MAX_WIDTH));
        drag(&mut cx, 260., 10.);
        cx.update(|_, cx| assert_eq!(view.read(cx).repos_panel_width, REPOS_MIN_WIDTH));

        // The drag ends persisted both widths.
        cx.update(|_, _| {
            let settings = crate::store::settings().unwrap();
            assert_eq!(settings.panels.repos, Some(160));
            assert_eq!(settings.panels.prs, Some(480));
        });

        let _ = std::fs::remove_dir_all(&home);
    }

    /// Selecting a repository in the left panel switches the middle list to
    /// its PRs and selects the first one.
    #[gpui_kit::test]
    fn selecting_a_repo_switches_the_pr_list(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        cx.update(bind_keys);
        let (handle, view) = open_app(cx, vec![pr("acme/api", 1, "one"), pr("acme/web", 2, "two")]);
        cx.update_window(handle, |_, window, cx| {
            focus_window(&view, window, cx);
            window.render_frame(cx);
        })
        .unwrap();

        cx.update(|cx| {
            let view = view.read(cx);
            assert_eq!(view.state.selected_repo.as_deref(), Some("acme/api"));
            assert_eq!(view.state.prs.len(), 1);
            assert_eq!(view.state.selected_pr().map(|pr| pr.number), Some(1));
        });

        // Click the acme/web row in the repositories panel.
        cx.update_window(handle, |_, window, cx| {
            window.click("repo-acme/web", cx);
            window.render_frame(cx);
        })
        .unwrap();

        cx.update(|cx| {
            let view = view.read(cx);
            assert_eq!(view.state.selected_repo.as_deref(), Some("acme/web"));
            assert_eq!(view.state.prs.len(), 1);
            assert_eq!(view.state.prs[0].number, 2);
            assert_eq!(view.state.selected_pr().map(|pr| pr.number), Some(2));
        });
    }

    /// The settings dialog opens from the repositories panel and saves the
    /// chosen theme and comment template.
    #[gpui_kit::test]
    fn the_settings_dialog_saves_theme_and_template(cx: &mut TestAppContext) {
        let home = std::env::temp_dir().join(format!("diffy-settings-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        unsafe { std::env::set_var("DIFFY_HOME", &home) };

        cx.update(gpui_kit::init);
        cx.update(bind_keys);
        let (handle, view) = open_app(cx, vec![pr("acme/api", 1, "one")]);

        cx.update_window(handle, |_, window, cx| {
            focus_window(&view, window, cx);
            view.update(cx, |this, cx| this.open_settings_dialog(window, cx));
            window.render_frame(cx);
        })
        .unwrap();

        cx.update_window(handle, |_, window, cx| {
            let buttons = window.find_all_by_role(Role::Button);
            let labels: Vec<Option<&str>> = buttons.iter().map(|b| b.label()).collect();
            assert!(
                labels.contains(&Some("Save")),
                "the settings dialog did not open, no Save button: {labels:?}"
            );
            // The alert dialog's confirm button carries the id "ok".
            window.click("ok", cx);
            window.render_frame(cx);
        })
        .unwrap();

        cx.update(|cx| {
            let settings = crate::store::settings().unwrap();
            assert_eq!(settings.theme, crate::store::ThemePref::Light);
            assert_eq!(settings.comment_template, None);
            let _ = cx;
        });

        let _ = std::fs::remove_dir_all(&home);
    }

    /// The saved theme preference drives the palette: applying dark switches
    /// the mode, applying light switches back.
    #[gpui_kit::test]
    fn the_theme_preference_switches_the_palette(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        cx.update(|cx| {
            crate::theme::apply(crate::store::ThemePref::Dark, cx);
            assert_eq!(
                gpui_kit::component::Theme::global(cx).mode,
                gpui_kit::component::ThemeMode::Dark
            );
            crate::theme::apply(crate::store::ThemePref::Light, cx);
            assert_eq!(
                gpui_kit::component::Theme::global(cx).mode,
                gpui_kit::component::ThemeMode::Light
            );
        });
    }

    /// The diff panel paints syntax colors only when a grammar is compiled in:
    /// the file's language is detected from its path, and the matching
    /// tree-sitter grammar must be registered. Dropping the `tree-sitter-*`
    /// features from the gpui-kit dependency fails this test.
    #[test]
    fn diff_files_detect_their_language_and_have_a_compiled_grammar() {
        let files = DiffFile::parse(PATCH).expect("patch parses");
        let file = &files[0];
        assert_eq!(file.path().as_ref(), "src/main.rs");
        assert_eq!(
            file.language().as_ref(),
            "rust",
            "the syntax language should come from the path extension"
        );
        let registry = gpui_kit::component::highlighter::LanguageRegistry::singleton();
        let grammar = registry
            .language("rust")
            .expect("rust is not a registered language");
        assert!(
            grammar.has_grammar(),
            "no rust grammar is compiled in; the diff renders without syntax colors"
        );
    }
}
