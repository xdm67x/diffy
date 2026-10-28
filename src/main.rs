mod api;
mod claude;
mod models;
mod review;
mod state;
mod store;
mod widgets;

use crate::state::{ReviewStatus, State};
use crate::widgets::{DetailWidget, PrListWidget};
use color_eyre::Result;
use ratatui::Frame;
use ratatui::crossterm::event::{self, Event, KeyCode, MouseEvent, MouseEventKind};
use ratatui::crossterm::execute;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph, Wrap};
use std::io::stdout;
use std::sync::mpsc;
use std::time::Duration;

fn main() -> Result<()> {
    color_eyre::install()?;

    let prs = api::get_review_prs()?;
    let mut state = State::new(prs);
    match store::load_reviews() {
        Ok(saved) => state.reviews.extend(
            saved
                .into_iter()
                .map(|(url, report)| (url, ReviewStatus::Done(report))),
        ),
        Err(error) => state.notify(format!("could not load saved reviews: {error:#}")),
    }
    let (tx, rx) = mpsc::channel();

    execute!(stdout(), event::EnableMouseCapture)?;
    let result = ratatui::run(|terminal| {
        loop {
            while let Ok(event) = rx.try_recv() {
                if let review::ReviewEvent::Done { url, report } = &event
                    && let Err(error) = store::save_review(url, report)
                {
                    state.notify(format!("could not save review: {error:#}"));
                }
                state.apply(event);
            }
            state.expire_toast();

            terminal.draw(|frame| render(frame, &mut state))?;

            if !event::poll(Duration::from_millis(100))? {
                continue;
            }
            match event::read()? {
                Event::Mouse(mouse) => handle_mouse(&mut state, mouse),
                other => {
                    let Some(key) = other.as_key_press_event() else {
                        continue;
                    };
                    if state.filter_input {
                        match key.code {
                            KeyCode::Esc => {
                                state.filter_input = false;
                                state.clear_filter();
                            }
                            KeyCode::Enter => state.filter_input = false,
                            KeyCode::Backspace => state.filter_pop(),
                            KeyCode::Char(character) => state.filter_push(character),
                            _ => {}
                        }
                        continue;
                    }

                    match key.code {
                        KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                        KeyCode::Char('j') | KeyCode::Down => state.select_next(),
                        KeyCode::Char('k') | KeyCode::Up => state.select_previous(),
                        KeyCode::Char('g') | KeyCode::Home => state.select_first(),
                        KeyCode::Char('G') | KeyCode::End => state.select_last(),
                        KeyCode::Char('J') | KeyCode::PageDown => state.scroll_detail(5),
                        KeyCode::Char('K') | KeyCode::PageUp => state.scroll_detail(-5),
                        KeyCode::Char('/') => state.filter_input = true,
                        KeyCode::Char('c') => state.clear_filter(),
                        KeyCode::Enter | KeyCode::Char('r') => state.spawn_review(&tx),
                        KeyCode::Char('R') => match api::get_review_prs() {
                            Ok(prs) => {
                                state.toast = None;
                                state.set_prs(prs);
                            }
                            Err(error) => state.notify(format!("{error:#}")),
                        },
                        _ => {}
                    }
                }
            }
        }
    });
    let _ = execute!(stdout(), event::DisableMouseCapture);
    result
}

/// Wheel over the list moves the selection; wheel over the detail pane scrolls the PR text.
fn handle_mouse(state: &mut State, mouse: MouseEvent) {
    let delta = match mouse.kind {
        MouseEventKind::ScrollUp => -3,
        MouseEventKind::ScrollDown => 3,
        _ => return,
    };
    if state.detail_area.contains((mouse.column, mouse.row).into()) {
        state.scroll_detail(delta);
    } else if state.list_area.contains((mouse.column, mouse.row).into()) {
        if delta > 0 {
            state.select_next();
        } else {
            state.select_previous();
        }
    }
}

fn render(frame: &mut Frame, state: &mut State) {
    let [header_area, body_area, footer_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(frame.area());
    let [list_area, detail_area] =
        Layout::horizontal([Constraint::Percentage(42), Constraint::Percentage(58)])
            .areas(body_area);
    state.list_area = list_area;
    state.detail_area = detail_area;

    let visible_repos = state
        .rows
        .iter()
        .filter(|row| matches!(row, crate::state::Row::Header { .. }))
        .count();
    let mut header = vec![
        Span::styled(
            " Diffy ",
            Style::new().fg(Color::Black).bg(Color::Cyan).bold(),
        ),
        Span::raw(" "),
        Span::styled(
            format!(
                "{} review request{} across {} repo{}",
                state.prs.len(),
                plural(state.prs.len()),
                visible_repos,
                plural(visible_repos)
            ),
            Style::new().fg(Color::White),
        ),
    ];
    if state.active.is_some() {
        header.push(Span::styled(
            "  ● review running",
            Style::new().fg(Color::Yellow),
        ));
    }
    if !state.filter.is_empty() {
        header.push(Span::styled(
            format!("  /{}/", state.filter),
            Style::new().fg(Color::Yellow),
        ));
    }
    frame.render_widget(Paragraph::new(Line::from(header)), header_area);

    let mut list_state = state.list_state;
    frame.render_stateful_widget(
        PrListWidget::new(&state.rows, &state.prs, &state.reviews, state.selected_row),
        list_area,
        &mut list_state,
    );
    state.list_state = list_state;

    let pr = state.selected_pr();
    let status = pr.and_then(|pr| state.review_status(pr));
    frame.render_widget(
        DetailWidget::new(pr, status, state.detail_scroll),
        detail_area,
    );

    let help = if state.filter_input {
        " repo filter: type to filter   Enter apply   Esc cancel   Backspace delete".to_string()
    } else {
        " j/k move   / filter repos   Enter/r review   J/K or wheel scroll   R refresh   q quit"
            .to_string()
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            help,
            Style::new().fg(Color::DarkGray).add_modifier(Modifier::DIM),
        ))),
        footer_area,
    );

    if let Some(toast) = &state.toast {
        render_toast(frame, body_area, &toast.message);
    }
}

fn render_toast(frame: &mut Frame, area: Rect, message: &str) {
    let width = area.width.saturating_sub(4).clamp(8, 76).min(area.width);
    let text_width = width.saturating_sub(4).max(1) as usize;
    let mut height: u16 = 2;
    for line in message.lines() {
        height += (line.chars().count().div_ceil(text_width)).max(1) as u16;
    }
    let height = height.min(area.height);
    let toast_area = Rect {
        x: area.x + area.width.saturating_sub(width + 1),
        y: area.y + area.height.saturating_sub(height + 1),
        width,
        height,
    };
    frame.render_widget(Clear, toast_area);
    frame.render_widget(
        Paragraph::new(message.to_string())
            .wrap(Wrap { trim: false })
            .style(Style::new().fg(Color::Red))
            .block(
                Block::bordered()
                    .title(" Error ")
                    .title_style(Style::new().fg(Color::Red).bold())
                    .border_style(Style::new().fg(Color::Red)),
            ),
        toast_area,
    );
}

fn plural(count: usize) -> &'static str {
    if count == 1 { "" } else { "s" }
}
