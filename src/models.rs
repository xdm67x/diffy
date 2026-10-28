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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
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

#[derive(Clone, Debug)]
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
    fn falls_back_to_url_for_repo_name() {
        let mut pull = pr(1, "", "https://github.com/acme/widgets/pull/1");
        pull.repository.name_with_owner.clear();
        assert_eq!(pull.repo_name(), "acme/widgets");
    }
}
