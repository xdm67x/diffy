mod api;
mod claude;
mod models;
mod review;
mod state;
mod widgets;

use crate::state::State;
use crate::widgets::{DetailWidget, PrListWidget};
use color_eyre::Result;
use ratatui::Frame;
use ratatui::crossterm::event::{self, KeyCode};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use std::sync::mpsc;
use std::time::Duration;

fn main() -> Result<()> {
    color_eyre::install()?;

    let prs = api::get_review_prs()?;
    let mut state = State::new(prs);
    let (tx, rx) = mpsc::channel();

    ratatui::run(|terminal| {
        loop {
            while let Ok(event) = rx.try_recv() {
                state.apply(event);
            }

            terminal.draw(|frame| render(frame, &mut state))?;

            if !event::poll(Duration::from_millis(100))? {
                continue;
            }
            let Some(key) = event::read()?.as_key_press_event() else {
                continue;
            };

            match key.code {
                KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                KeyCode::Char('j') | KeyCode::Down => state.select_next(),
                KeyCode::Char('k') | KeyCode::Up => state.select_previous(),
                KeyCode::Char('g') | KeyCode::Home => state.select_first(),
                KeyCode::Char('G') | KeyCode::End => state.select_last(),
                KeyCode::Char('J') | KeyCode::PageDown => state.scroll_detail(5),
                KeyCode::Char('K') | KeyCode::PageUp => state.scroll_detail(-5),
                KeyCode::Enter | KeyCode::Char('r') => state.spawn_review(&tx),
                KeyCode::Char('R') => match api::get_review_prs() {
                    Ok(prs) => {
                        state.notice = None;
                        state.set_prs(prs);
                    }
                    Err(error) => state.notice = Some(format!("{error:#}")),
                },
                _ => {}
            }
        }
    })
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
                state.groups.len(),
                plural(state.groups.len())
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
    if let Some(notice) = &state.notice {
        header.push(Span::styled(
            format!("  {notice}"),
            Style::new().fg(Color::Red),
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

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            " j/k move   Enter/r review   J/K scroll   R refresh   q quit",
            Style::new().fg(Color::DarkGray).add_modifier(Modifier::DIM),
        ))),
        footer_area,
    );
}

fn plural(count: usize) -> &'static str {
    if count == 1 { "" } else { "s" }
}
