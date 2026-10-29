use crate::models::PullRequest;
use crate::state::{ReviewStatus, Row};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, List, ListItem, ListState, StatefulWidget};
use std::collections::HashMap;

pub struct PrListWidget<'a> {
    rows: &'a [Row],
    prs: &'a [PullRequest],
    reviews: &'a HashMap<String, Vec<ReviewStatus>>,
    progress: &'a HashMap<String, String>,
    selected_row: usize,
}

impl<'a> PrListWidget<'a> {
    pub fn new(
        rows: &'a [Row],
        prs: &'a [PullRequest],
        reviews: &'a HashMap<String, Vec<ReviewStatus>>,
        progress: &'a HashMap<String, String>,
        selected_row: usize,
    ) -> Self {
        Self {
            rows,
            prs,
            reviews,
            progress,
            selected_row,
        }
    }
}

impl StatefulWidget for PrListWidget<'_> {
    type State = ListState;

    fn render(self, area: Rect, buf: &mut Buffer, list_state: &mut Self::State) {
        let mut items = Vec::with_capacity(self.rows.len());

        for (index, row) in self.rows.iter().enumerate() {
            match row {
                Row::Header { repo, count } => {
                    let mut lines = Vec::new();
                    if index > 0 {
                        lines.push(Line::default());
                    }
                    lines.push(Line::from(vec![
                        Span::styled("▾ ", Style::new().fg(Color::DarkGray)),
                        Span::styled(repo.clone(), Style::new().fg(Color::Cyan).bold()),
                        Span::styled(format!("  {count}"), Style::new().fg(Color::DarkGray)),
                    ]));
                    items.push(ListItem::new(lines));
                }
                Row::Pr(pr_index) => {
                    let Some(pr) = self.prs.get(*pr_index) else {
                        continue;
                    };
                    let (marker, color, status) =
                        match self.reviews.get(&pr.url).and_then(|history| history.last()) {
                            None => ("○", Color::DarkGray, "not reviewed".to_string()),
                            Some(ReviewStatus::Running(stage)) => (
                                "●",
                                Color::Yellow,
                                self.progress
                                    .get(&pr.url)
                                    .cloned()
                                    .unwrap_or_else(|| stage.label().to_string()),
                            ),
                            Some(ReviewStatus::Done(report)) => match report.verdict {
                                crate::models::Verdict::Merge => {
                                    ("✔", Color::Green, "merge".to_string())
                                }
                                crate::models::Verdict::RequestChanges => {
                                    ("✖", Color::Red, "changes requested".to_string())
                                }
                            },
                            Some(ReviewStatus::Failed(_)) => {
                                ("✖", Color::Red, "review failed".to_string())
                            }
                        };

                    let mut spans = vec![
                        Span::styled(format!("{marker} "), Style::new().fg(color)),
                        Span::styled(format!("#{}", pr.number), Style::new().fg(Color::DarkGray)),
                        Span::raw(" "),
                        Span::styled(pr.title.clone(), Style::new().fg(Color::White)),
                    ];
                    if pr.is_draft {
                        spans.push(Span::styled(" [draft]", Style::new().fg(Color::DarkGray)));
                    }
                    spans.push(Span::styled(
                        format!("  {status}"),
                        Style::new().fg(color).add_modifier(Modifier::DIM),
                    ));

                    items.push(ListItem::new(Line::from(spans)));
                }
            }
        }

        list_state.select((!self.rows.is_empty()).then_some(self.selected_row));
        let list = List::new(items)
            .block(
                Block::bordered()
                    .title(" Review requests ")
                    .border_style(Style::new().fg(Color::DarkGray)),
            )
            .highlight_style(
                Style::new()
                    .bg(Color::Indexed(238))
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol("› ");

        StatefulWidget::render(list, area, buf, list_state);
    }
}
