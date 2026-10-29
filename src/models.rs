use serde::{Deserialize, Deserializer, Serialize};

#[derive(Clone, Debug, Deserialize)]
pub struct PullRequest {
    pub number: u64,
    pub title: String,
    pub url: String,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub repository: Repository,
    #[serde(default)]
    pub author: Author,
    #[serde(default, rename = "isDraft")]
    pub is_draft: bool,
    #[serde(default, rename = "updatedAt")]
    pub updated_at: String,
    #[serde(default, rename = "commentsCount")]
    pub comments_count: u64,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct Repository {
    #[serde(default, rename = "nameWithOwner")]
    pub name_with_owner: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct Author {
    #[serde(default)]
    pub login: String,
}

impl PullRequest {
    /// `repository.nameWithOwner` is not always populated by `gh search prs`,
    /// so fall back to the URL path.
    pub fn repo_name(&self) -> String {
        if !self.repository.name_with_owner.is_empty() {
            return self.repository.name_with_owner.clone();
        }
        let path = self.url.split("github.com/").nth(1).unwrap_or("");
        path.split("/pull/").next().unwrap_or("unknown").to_string()
    }

    /// True when `gh` reports PR activity after `epoch`, such as a push or a
    /// comment after a review.
    pub fn updated_after(&self, epoch: u64) -> bool {
        crate::time::parse_utc(&self.updated_at).is_some_and(|updated| updated > epoch as i64)
    }

    pub fn description(&self) -> &str {
        if self.body.trim().is_empty() {
            "(the PR has no description)"
        } else {
            self.body.trim()
        }
    }
}

#[derive(Clone, Debug)]
pub struct PrGroup {
    pub repo: String,
    pub prs: Vec<PullRequest>,
}

pub fn group_by_repo(mut prs: Vec<PullRequest>) -> Vec<PrGroup> {
    prs.sort_by(|a, b| {
        a.repo_name()
            .cmp(&b.repo_name())
            .then_with(|| b.number.cmp(&a.number))
    });
    prs.dedup_by(|a, b| a.url == b.url);

    let mut groups: Vec<PrGroup> = Vec::new();
    for pr in prs {
        let repo = pr.repo_name();
        match groups.last_mut() {
            Some(group) if group.repo == repo => group.prs.push(pr),
            _ => groups.push(PrGroup {
                repo,
                prs: vec![pr],
            }),
        }
    }
    groups
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct ReviewerOutput {
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub findings: Vec<Finding>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Finding {
    #[serde(default)]
    pub file: String,
    #[serde(default, deserialize_with = "de_optional_u64")]
    pub line: Option<u64>,
    #[serde(default)]
    pub severity: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub detail: String,
    #[serde(default)]
    pub intent_related: bool,
}

impl Finding {
    pub fn location(&self) -> String {
        match (&self.file.is_empty(), self.line) {
            (false, Some(line)) => format!("{}:{}", self.file, line),
            (false, None) => self.file.clone(),
            (true, _) => "general".to_string(),
        }
    }

    pub fn severity(&self) -> String {
        if self.severity.trim().is_empty() {
            "medium".to_string()
        } else {
            self.severity.to_lowercase()
        }
    }

    /// Worst first: high, medium, low, anything else.
    pub fn severity_rank(&self) -> u8 {
        match self.severity().as_str() {
            "high" => 0,
            "medium" => 1,
            "low" => 2,
            _ => 3,
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct JudgeDecision {
    #[serde(default, deserialize_with = "de_usize")]
    pub index: usize,
    #[serde(default)]
    pub relevant: bool,
    #[serde(default)]
    pub reason: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct JudgeOutput {
    #[serde(default)]
    pub verdict: String,
    #[serde(default)]
    pub intent: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub singles: Vec<JudgeDecision>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Merge,
    RequestChanges,
}

impl From<&str> for Verdict {
    fn from(value: &str) -> Self {
        match value.trim().to_lowercase().as_str() {
            "merge" | "approve" | "approved" => Verdict::Merge,
            _ => Verdict::RequestChanges,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ReviewReport {
    pub verdict: Verdict,
    pub summary: String,
    pub intent: String,
    /// Findings independently reported by both reviewers.
    pub confirmed: Vec<Finding>,
    /// Single-reviewer findings accepted by the judge.
    pub accepted: Vec<Finding>,
    /// Single-reviewer findings the judge discarded.
    pub dismissed: Vec<(Finding, String)>,
}

impl ReviewReport {
    /// Markdown body posted to GitHub when the review is submitted. Written for a
    /// human skimming the PR: verdict first, then findings worst-first.
    pub fn to_markdown(&self) -> String {
        let (emoji, verdict) = match self.verdict {
            Verdict::Merge => ("✅", "Approve"),
            Verdict::RequestChanges => ("❌", "Request changes"),
        };
        let findings: Vec<(&Finding, &str)> = self
            .confirmed
            .iter()
            .map(|finding| (finding, "reported by both reviewers"))
            .chain(
                self.accepted
                    .iter()
                    .map(|finding| (finding, "accepted by the judge")),
            )
            .collect();

        let mut out = format!("## {emoji} Diffy review: {verdict}\n\n");
        if !self.summary.trim().is_empty() {
            out.push_str(self.summary.trim());
            out.push_str("\n\n");
        }
        if !self.intent.trim().is_empty() {
            out.push_str(&format!("> **Intent:** {}\n\n", self.intent.trim()));
        }

        if findings.is_empty() {
            out.push_str("No blocking findings.\n");
        } else {
            out.push_str(&format!(
                "**{} {} to address**\n",
                findings.len(),
                if findings.len() == 1 {
                    "finding"
                } else {
                    "findings"
                }
            ));
            for (label, group) in severity_groups(&findings) {
                out.push_str(&format!("\n#### {label}\n\n"));
                for (finding, source) in group {
                    out.push_str(&format!(
                        "1. **{}** — `{}`\n",
                        finding.title.trim(),
                        finding.location()
                    ));
                    for line in finding.detail.trim().lines() {
                        out.push_str(&format!("   {line}\n"));
                    }
                    out.push_str(&format!("   <sub>{source}</sub>\n"));
                }
            }
        }

        let dismissed = self.dismissed.len();
        if dismissed > 0 {
            out.push_str(&format!(
                "\n---\n\n<sub>{dismissed} other finding{} filtered out as not relevant.</sub>\n",
                if dismissed == 1 { "" } else { "s" }
            ));
        }
        out
    }
}

/// Findings grouped high → medium → low → anything else, worst first.
fn severity_groups<'a>(
    findings: &[(&'a Finding, &'a str)],
) -> Vec<(&'static str, Vec<(&'a Finding, &'a str)>)> {
    let mut groups = Vec::new();
    for (severity, label) in [
        ("high", "🔴 High severity"),
        ("medium", "🟠 Medium severity"),
        ("low", "🟡 Low severity"),
        ("", "⚪ Other"),
    ] {
        let group: Vec<_> = findings
            .iter()
            .copied()
            .filter(|(finding, _)| {
                let actual = finding.severity();
                if severity.is_empty() {
                    !matches!(actual.as_str(), "high" | "medium" | "low")
                } else {
                    actual == severity
                }
            })
            .collect();
        if !group.is_empty() {
            groups.push((label, group));
        }
    }
    groups
}

fn de_optional_u64<'de, D>(deserializer: D) -> Result<Option<u64>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    Ok(value.and_then(|value| match value {
        serde_json::Value::Number(number) => number.as_u64(),
        serde_json::Value::String(text) => text.trim().parse().ok(),
        _ => None,
    }))
}

fn de_usize<'de, D>(deserializer: D) -> Result<usize, D::Error>
where
    D: Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(match value {
        serde_json::Value::Number(number) => number.as_u64().unwrap_or(0) as usize,
        serde_json::Value::String(text) => text
            .trim()
            .trim_start_matches('#')
            .trim_start_matches(['S', 's'])
            .parse()
            .unwrap_or(0),
        _ => 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pr(number: u64, repo: &str, url: &str) -> PullRequest {
        PullRequest {
            number,
            title: format!("PR {number}"),
            url: url.to_string(),
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

    #[test]
    fn groups_and_sorts_by_repo_then_number_desc() {
        let groups = group_by_repo(vec![
            pr(1, "b/b", "https://github.com/b/b/pull/1"),
            pr(2, "a/a", "https://github.com/a/a/pull/2"),
            pr(3, "a/a", "https://github.com/a/a/pull/3"),
        ]);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].repo, "a/a");
        assert_eq!(groups[0].prs[0].number, 3);
        assert_eq!(groups[1].repo, "b/b");
    }

    #[test]
    fn removes_duplicate_prs() {
        let groups = group_by_repo(vec![
            pr(1, "a/a", "https://github.com/a/a/pull/1"),
            pr(1, "a/a", "https://github.com/a/a/pull/1"),
        ]);
        assert_eq!(groups[0].prs.len(), 1);
    }

    #[test]
    fn accepts_line_and_judge_index_as_strings() {
        let finding: Finding = serde_json::from_str(
            r#"{"file":"a.rs","line":"12","severity":"high","title":"x","detail":"y"}"#,
        )
        .unwrap();
        assert_eq!(finding.line, Some(12));

        let decision: JudgeDecision =
            serde_json::from_str(r#"{"index":"S2","relevant":true}"#).unwrap();
        assert_eq!(decision.index, 2);
    }

    #[test]
    fn markdown_body_carries_verdict_and_actionable_findings() {
        let report = ReviewReport {
            verdict: Verdict::RequestChanges,
            summary: "needs work".to_string(),
            intent: "partially implemented".to_string(),
            confirmed: vec![Finding {
                file: "src/db.rs".to_string(),
                line: Some(7),
                severity: "high".to_string(),
                title: "SQL injection".to_string(),
                detail: "user input is concatenated".to_string(),
                intent_related: true,
            }],
            accepted: Vec::new(),
            dismissed: vec![(Finding::default(), "speculation".to_string())],
        };
        let body = report.to_markdown();
        assert!(body.starts_with("## ❌ Diffy review: Request changes"));
        assert!(body.contains("> **Intent:** partially implemented"));
        assert!(body.contains("**1 finding to address**"));
        assert!(body.contains("#### 🔴 High severity"));
        assert!(body.contains("1. **SQL injection** — `src/db.rs:7`"));
        assert!(body.contains("   user input is concatenated"));
        assert!(body.contains("<sub>reported by both reviewers</sub>"));
        assert!(body.contains("<sub>1 other finding filtered out as not relevant.</sub>"));
        assert!(!body.contains("speculation"));
    }

    #[test]
    fn markdown_orders_severities_and_reads_cleanly_without_findings() {
        let finding = |severity: &str, file: &str| Finding {
            file: file.to_string(),
            severity: severity.to_string(),
            title: severity.to_string(),
            ..Finding::default()
        };
        let report = ReviewReport {
            verdict: Verdict::Merge,
            summary: String::new(),
            intent: String::new(),
            confirmed: vec![finding("low", "c.rs"), finding("weird", "d.rs")],
            accepted: vec![finding("high", "a.rs")],
            dismissed: Vec::new(),
        };
        let body = report.to_markdown();
        let order: Vec<usize> = [
            "#### 🔴 High severity",
            "#### 🟡 Low severity",
            "#### ⚪ Other",
        ]
        .iter()
        .map(|heading| body.find(heading).expect("missing severity heading"))
        .collect();
        assert!(order.is_sorted());
        assert!(body.contains("`a.rs`") && body.contains("`c.rs`") && body.contains("`d.rs`"));

        let clean = ReviewReport {
            verdict: Verdict::Merge,
            summary: "ship it".to_string(),
            intent: String::new(),
            confirmed: Vec::new(),
            accepted: Vec::new(),
            dismissed: Vec::new(),
        }
        .to_markdown();
        assert!(clean.starts_with("## ✅ Diffy review: Approve"));
        assert!(clean.contains("No blocking findings."));
        assert!(!clean.contains("filtered out"));
    }

    #[test]
    fn update_time_marks_an_older_review_as_stale() {
        let mut pull = pr(1, "a/a", "https://github.com/a/a/pull/1");
        pull.updated_at = "2026-10-29T07:00:00Z".to_string();
        assert!(
            pull.updated_after(1_793_000_000),
            "the PR moved after the review"
        );
        assert!(
            !pull.updated_after(1_793_259_474),
            "a review after the push is current"
        );
        pull.updated_at.clear();
        assert!(
            !pull.updated_after(0),
            "a missing timestamp cannot be stale"
        );
    }

    #[test]
    fn severity_rank_orders_worst_first() {
        let rank = |severity: &str| {
            Finding {
                severity: severity.to_string(),
                ..Finding::default()
            }
            .severity_rank()
        };
        assert!(rank("high") < rank("medium"));
        assert!(rank("medium") < rank("low"));
        assert!(rank("low") < rank("odd"));
        assert_eq!(rank(""), rank("medium"), "missing severity reads as medium");
    }

    #[test]
    fn falls_back_to_url_for_repo_name() {
        let mut pull = pr(1, "", "https://github.com/acme/widgets/pull/1");
        pull.repository.name_with_owner.clear();
        assert_eq!(pull.repo_name(), "acme/widgets");
    }
}
