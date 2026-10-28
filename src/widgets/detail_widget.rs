use crate::models::{Finding, PullRequest, ReviewReport};
use crate::state::ReviewStatus;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph, Widget, Wrap};

pub struct DetailWidget<'a> {
    pr: Option<&'a PullRequest>,
    status: Option<&'a ReviewStatus>,
    scroll: u16,
}

impl<'a> DetailWidget<'a> {
    pub fn new(pr: Option<&'a PullRequest>, status: Option<&'a ReviewStatus>, scroll: u16) -> Self {
        Self { pr, status, scroll }
    }
}

impl Widget for DetailWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let title = self
            .pr
            .map(|pr| format!(" PR #{} ", pr.number))
            .unwrap_or_else(|| " Pull request ".to_string());
        let block = Block::bordered()
            .title(title)
            .border_style(Style::new().fg(Color::DarkGray));
        let mut lines: Vec<Line<'_>> = Vec::new();

        match self.pr {
            None => {
                lines.push(Line::from(Span::styled(
                    "No pull request selected. Press R to refresh.",
                    Style::new().fg(Color::DarkGray),
                )));
            }
            Some(pr) => {
                lines.push(Line::from(Span::styled(
                    format!("#{} {}", pr.number, pr.title),
                    Style::new().fg(Color::White).bold(),
                )));
                lines.push(Line::from(vec![
                    Span::styled(pr.repo_name(), Style::new().fg(Color::Cyan)),
                    Span::styled(
                        format!("  ·  by {}  ·  updated {}", author_name(pr), pr.updated_at),
                        Style::new().fg(Color::DarkGray),
                    ),
                ]));
                lines.push(Line::from(Span::styled(
                    pr.url.clone(),
                    Style::new().fg(Color::Blue),
                )));
                if pr.is_draft {
                    lines.push(Line::from(Span::styled(
                        "DRAFT",
                        Style::new().fg(Color::Yellow).bold(),
                    )));
                }

                lines.push(Line::default());
                lines.push(heading("Description"));
                lines.extend(tui_markdown::from_str(pr.description()).lines);

                lines.push(Line::default());
                lines.push(heading("Review"));
                push_status(&mut lines, self.status);
                lines.push(Line::default());
                lines.push(Line::from(Span::styled(
                    "The reviewer council runs two independent reviewer agents in parallel \
                     with gh/git access to inspect the PR branch, then a judge confirms only \
                     issues both found and rules on the rest. The verdict checks the diff \
                     against the PR description.",
                    Style::new().fg(Color::DarkGray),
                )));
            }
        }

        Paragraph::new(lines)
            .block(block)
            .scroll((self.scroll, 0))
            .wrap(Wrap { trim: false })
            .render(area, buf);
    }
}

fn author_name(pr: &PullRequest) -> &str {
    if pr.author.login.is_empty() {
        "unknown"
    } else {
        &pr.author.login
    }
}

fn heading(text: &str) -> Line<'static> {
    Line::from(Span::styled(
        text.to_string(),
        Style::new()
            .fg(Color::White)
            .bold()
            .add_modifier(Modifier::UNDERLINED),
    ))
}

fn push_status(lines: &mut Vec<Line<'_>>, status: Option<&ReviewStatus>) {
    match status {
        None => lines.push(Line::from(Span::styled(
            "Not reviewed yet. Press Enter or r to run the review council.",
            Style::new().fg(Color::DarkGray),
        ))),
        Some(ReviewStatus::Running(stage)) => {
            lines.push(Line::from(Span::styled(
                format!("● {}", stage.label()),
                Style::new().fg(Color::Yellow).bold(),
            )));
            lines.push(Line::from(Span::styled(
                "Claude is working. The PR list updates when it finishes.",
                Style::new().fg(Color::DarkGray),
            )));
        }
        Some(ReviewStatus::Failed(error)) => {
            lines.push(Line::from(Span::styled(
                "✖ Review failed",
                Style::new().fg(Color::Red).bold(),
            )));
            for line in error.lines() {
                lines.push(Line::from(Span::styled(
                    line.to_string(),
                    Style::new().fg(Color::Red),
                )));
            }
        }
        Some(ReviewStatus::Done(report)) => push_report(lines, report),
    }
}

fn push_report(lines: &mut Vec<Line<'_>>, report: &ReviewReport) {
    let (label, color) = match report.verdict {
        crate::models::Verdict::Merge => ("VERDICT: MERGE", Color::Green),
        crate::models::Verdict::RequestChanges => ("VERDICT: REQUEST CHANGES", Color::Red),
    };
    lines.push(Line::from(Span::styled(
        label,
        Style::new()
            .fg(color)
            .bold()
            .add_modifier(Modifier::REVERSED),
    )));

    if !report.intent.trim().is_empty() {
        lines.push(Line::default());
        lines.push(Line::from(Span::styled(
            "Intent",
            Style::new().fg(Color::Cyan).bold(),
        )));
        lines.push(Line::from(report.intent.clone()));
    }

    if !report.summary.trim().is_empty() {
        lines.push(Line::default());
        lines.push(Line::from(Span::styled(
            "Summary",
            Style::new().fg(Color::Cyan).bold(),
        )));
        lines.push(Line::from(report.summary.clone()));
    }

    if !report.confirmed.is_empty() {
        lines.push(Line::default());
        lines.push(Line::from(Span::styled(
            format!("Confirmed by both reviewers ({})", report.confirmed.len()),
            Style::new().fg(Color::Cyan).bold(),
        )));
        for finding in &report.confirmed {
            push_finding(lines, finding);
        }
    }

    if !report.accepted.is_empty() {
        lines.push(Line::default());
        lines.push(Line::from(Span::styled(
            format!("Accepted by the judge ({})", report.accepted.len()),
            Style::new().fg(Color::Cyan).bold(),
        )));
        for finding in &report.accepted {
            push_finding(lines, finding);
        }
    }

    if !report.dismissed.is_empty() {
        lines.push(Line::default());
        lines.push(Line::from(Span::styled(
            format!("Dismissed as not relevant ({})", report.dismissed.len()),
            Style::new().fg(Color::DarkGray).bold(),
        )));
        for (finding, reason) in &report.dismissed {
            let mut text = format!("  • {} — {}", finding.title, finding.location());
            if !reason.trim().is_empty() {
                text.push_str(&format!(" — {reason}"));
            }
            lines.push(Line::from(Span::styled(
                text,
                Style::new().fg(Color::DarkGray),
            )));
        }
    }

    if report.confirmed.is_empty() && report.accepted.is_empty() && report.dismissed.is_empty() {
        lines.push(Line::default());
        lines.push(Line::from(Span::styled(
            "No actionable findings were reported.",
            Style::new().fg(Color::Green),
        )));
    }
}

fn push_finding(lines: &mut Vec<Line<'_>>, finding: &Finding) {
    let color = match finding.severity().as_str() {
        "critical" | "high" => Color::Red,
        "medium" => Color::Yellow,
        _ => Color::Gray,
    };
    lines.push(Line::from(vec![
        Span::styled(
            format!("[{}] ", finding.severity().to_uppercase()),
            Style::new().fg(color).bold(),
        ),
        Span::styled(
            format!("{} — {}", finding.location(), finding.title),
            Style::new().fg(Color::White).bold(),
        ),
    ]));
    if !finding.detail.trim().is_empty() {
        for line in finding.detail.lines() {
            lines.push(Line::from(Span::styled(
                format!("    {line}"),
                Style::new().fg(Color::Gray),
            )));
        }
    }
}
