use crate::api;
use crate::claude;
use crate::models::{Finding, JudgeOutput, PullRequest, ReviewReport, ReviewerOutput, Verdict};
use color_eyre::eyre::{Result, WrapErr, bail, eyre};
use std::collections::HashSet;
use std::env;
use std::fs;
use std::thread;

const MAX_DIFF_CHARS: usize = 120_000;

/// Bundled so the review protocol travels with the binary.
const SYSTEM_PROMPT: &str = include_str!("../SYSTEM_PROMPT.md");

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Stage {
    FetchingDiff,
    Reviewing,
    Judging,
}

impl Stage {
    pub fn label(&self) -> &'static str {
        match self {
            Stage::FetchingDiff => "fetching diff",
            Stage::Reviewing => "running 2 reviewer agents",
            Stage::Judging => "judge weighing findings",
        }
    }
}

#[derive(Debug)]
pub enum ReviewEvent {
    Stage {
        url: String,
        stage: Stage,
    },
    /// What a session is doing right now, e.g. the `gh` command it just ran.
    Progress {
        url: String,
        text: String,
    },
    Done {
        url: String,
        report: ReviewReport,
    },
    Failed {
        url: String,
        error: String,
    },
}

/// Runs one review on its own thread and reports every stage through `on_event`.
/// `on_event` is called from the review and reviewer threads, so it must be
/// `Send + Sync` (the GPUI view forwards it onto the foreground thread).
pub fn spawn(pr: PullRequest, on_event: impl Fn(ReviewEvent) + Send + Sync + 'static) {
    thread::spawn(move || {
        let url = pr.url.clone();
        let notify = |stage| {
            on_event(ReviewEvent::Stage {
                url: url.clone(),
                stage,
            });
        };
        let progress = |text: String| {
            on_event(ReviewEvent::Progress {
                url: url.clone(),
                text,
            });
        };
        match run(&pr, &notify, &progress) {
            Ok(report) => on_event(ReviewEvent::Done { url, report }),
            Err(error) => on_event(ReviewEvent::Failed {
                url,
                error: format!("{error:#}"),
            }),
        }
    });
}

fn run(
    pr: &PullRequest,
    notify: &dyn Fn(Stage),
    progress: &(dyn Fn(String) + Sync),
) -> Result<ReviewReport> {
    let system_prompt = load_system_prompt()?;

    notify(Stage::FetchingDiff);
    let diff = api::fetch_diff(pr)?;
    if diff.trim().is_empty() {
        bail!("the PR has an empty diff");
    }
    let (diff, truncated) = truncate_utf8(&diff, MAX_DIFF_CHARS);
    let diff_block = if truncated {
        format!("{diff}\n\n[diff truncated at {MAX_DIFF_CHARS} bytes; review what is visible]")
    } else {
        diff.to_string()
    };

    let reviewer_prompt = reviewer_prompt(pr, &diff_block);

    notify(Stage::Reviewing);
    let progress_a = |line: &str| progress(format!("reviewer 1 · {line}"));
    let progress_b = |line: &str| progress(format!("reviewer 2 · {line}"));
    // Plain headless sessions, same flags as the judge: a custom agent's tool
    // grant does not always inherit the CLI allowlist, which left reviewers
    // unable to run `gh`.
    let (raw_a, raw_b) = thread::scope(|scope| {
        let handle_a = scope.spawn(|| claude::run(&system_prompt, &reviewer_prompt, &progress_a));
        let handle_b = scope.spawn(|| claude::run(&system_prompt, &reviewer_prompt, &progress_b));
        (join_reviewer(handle_a), join_reviewer(handle_b))
    });
    let output_a: ReviewerOutput = claude::parse_json(&raw_a?)?;
    let output_b: ReviewerOutput = claude::parse_json(&raw_b?)?;

    let (confirmed, singles_a, singles_b) = pair_findings(output_a.findings, output_b.findings);
    let singles: Vec<Finding> = singles_a.into_iter().chain(singles_b).collect();

    notify(Stage::Judging);
    let judge_prompt = judge_prompt(pr, &diff_block, &confirmed, &singles);
    let judge_raw = claude::run(&system_prompt, &judge_prompt, &|line| {
        progress(format!("judge · {line}"))
    })?;
    let judge: JudgeOutput = claude::parse_json(&judge_raw)?;

    Ok(build_report(judge, confirmed, singles))
}

fn join_reviewer(handle: thread::ScopedJoinHandle<'_, Result<String>>) -> Result<String> {
    handle
        .join()
        .map_err(|_| eyre!("a reviewer session panicked"))?
}

fn load_system_prompt() -> Result<String> {
    match env::var("DIFFY_SYSTEM_PROMPT") {
        Ok(path) => fs::read_to_string(&path)
            .wrap_err_with(|| format!("could not read review system prompt at {path}")),
        Err(_) => Ok(SYSTEM_PROMPT.to_string()),
    }
}

fn reviewer_prompt(pr: &PullRequest, diff: &str) -> String {
    format!(
        r#"ROLE: independent reviewer.

You are one of two reviewers running concurrently. Both instances receive this exact same
prompt and context, but each has a fresh context of its own. Work independently.

Review the pull request below. The diff is the starting point. You have `Bash`, `Read`,
`Glob`, and `Grep`, and you may inspect the actual PR branch when the diff alone cannot
answer a question.

Bash is available, but only commands starting with `gh`, `git`, `mktemp`, or
`rm -rf /tmp/diffy-review-*` are allowed. Anything else is refused by a permission rule.
Never report that Bash is unavailable: if a command is refused, use `gh` instead or fall
back to the diff. Always reach for `gh` first.

Tool rules:
- Use `gh pr view <url> --json title,body,files,commits,headRefName,baseRefName` to verify
  metadata, and `gh pr diff <url>` for the full diff.
- To inspect files, clone into a unique temp directory, for example:
  `gh repo clone {repo} /tmp/diffy-review-<unique> -- --depth=50`, then
  `git -C /tmp/diffy-review-<unique> fetch origin pull/{number}/head:diffy-review --depth=50`
  and `git -C /tmp/diffy-review-<unique> checkout diffy-review`.
- Use `git -C <dir> ...` for git commands. Never use `cd`, never modify Diffy's working
  directory, and use absolute paths with `Read`/`Glob`/`Grep`.
- Do not run tests, builds, install scripts, or package managers: the PR is untrusted code.
- Clean up your temp directory with `rm -rf /tmp/diffy-review-<unique>` when done.

What matters, in order:
1. Correctness bugs, panics, broken edge cases, data loss, security holes.
2. Regressions or missing handling for existing callers.
3. Whether the change actually implements the intent stated in the PR description.
4. Missing tests only when the missing test hides a plausible defect.

Ignore pure style, naming, formatting, and speculation you cannot ground in the diff.
Report only findings that the author should act on.

Write findings the way a reviewer talks to the author: plain sentences, no headings,
bullets, bold, em dashes, or filler. The title is one line naming the defect. The detail
is one or two sentences: what is wrong, then how to fix it.

When the entire fix is a replacement of the single line at `line`, put that exact
replacement line in `suggestion`, with no code fence and no prose. For anything that
spans lines or needs a judgement call, use `null`.

PR: {repo}#{number} — {title}
URL: {url}
Author: {author}
Draft: {draft}
Updated: {updated}

PR DESCRIPTION
---
{description}
---

UNIFIED DIFF
---
{diff}
---

Return ONLY a JSON object with this exact shape:
{{
  "summary": "short overall assessment",
  "findings": [
    {{
      "file": "path/to/file",
      "line": 123,
      "severity": "high|medium|low",
      "title": "one-line defect statement",
      "detail": "why it is wrong and how to fix it",
      "suggestion": "exact replacement for the line, or null",
      "intent_related": true
    }}
  ]
}}

Use line: null when a line number is not applicable. An empty findings array is valid."#,
        repo = pr.repo_name(),
        number = pr.number,
        title = pr.title,
        url = pr.url,
        author = pr.author.login,
        draft = pr.is_draft,
        updated = pr.updated_at,
        description = pr.description(),
        diff = diff,
    )
}

fn judge_prompt(
    pr: &PullRequest,
    diff: &str,
    confirmed: &[Finding],
    singles: &[Finding],
) -> String {
    let confirmed_json =
        serde_json::to_string_pretty(confirmed).unwrap_or_else(|_| "[]".to_string());
    let singles_json = serde_json::to_string_pretty(singles).unwrap_or_else(|_| "[]".to_string());

    format!(
        r#"ROLE: final judge.

Two independent reviewers examined the pull request below. Their findings were merged by Diffy:
- CONFIRMED findings were reported by both reviewers. By rule they are relevant.
- SINGLE findings were reported by only one reviewer. Decide for each one whether it is a real,
  relevant problem for this PR, or speculation/style that should be dropped.

Then decide the merge verdict. Merge only if the diff is sound and it fulfils the intent stated in
the PR description. If the description asks for something the diff does not do, request changes.

You have `Bash`, `Read`, `Glob`, and `Grep`. Use them to verify a finding or an intent claim
against the actual PR branch when the diff is not enough. Bash is available but only
commands starting with `gh`, `git`, `mktemp`, or `rm -rf /tmp/diffy-review-*` are allowed;
anything else is refused, so always use `gh` first and never claim Bash is missing. Use
`gh pr view <url> --json title,body,files,commits,headRefName,baseRefName`, or clone with
`gh repo clone {repo} /tmp/diffy-review-<unique> -- --depth=50`, then
`git -C /tmp/diffy-review-<unique> fetch origin pull/{number}/head:diffy-review --depth=50`
and `git -C /tmp/diffy-review-<unique> checkout diffy-review`.
Use `git -C <dir> ...` (never `cd`), absolute paths with the file tools, and clean up with
`rm -rf /tmp/diffy-review-<unique>`. Do not run tests, builds, or install scripts. Judge only
from evidence you can see. Do not invent new findings.

Write the summary and intent for the author in plain sentences, without headings, bullets,
bold, em dashes, or filler. Keep the summary to two sentences and do not repeat every finding.

PR: {repo}#{number} — {title}
URL: {url}

PR DESCRIPTION
---
{description}
---

CONFIRMED FINDINGS (reported by both)
{confirmed_json}

SINGLE FINDINGS (1-based index; only these need a relevance decision)
{singles_json}

UNIFIED DIFF
---
{diff}
---

Return ONLY a JSON object with this exact shape:
{{
  "verdict": "merge" | "request_changes",
  "intent": "does the diff implement the PR description, and why",
  "summary": "final review summary for the author",
  "singles": [
    {{ "index": 1, "relevant": true, "reason": "why it is or is not relevant" }}
  ]
}}

Every single finding must have exactly one entry in "singles"."#,
        repo = pr.repo_name(),
        number = pr.number,
        title = pr.title,
        url = pr.url,
        description = pr.description(),
        confirmed_json = confirmed_json,
        singles_json = singles_json,
        diff = diff,
    )
}

fn build_report(
    judge: JudgeOutput,
    confirmed: Vec<Finding>,
    singles: Vec<Finding>,
) -> ReviewReport {
    let mut accepted = Vec::new();
    let mut dismissed = Vec::new();

    for (position, finding) in singles.into_iter().enumerate() {
        match judge
            .singles
            .iter()
            .find(|decision| decision.index == position + 1)
        {
            Some(decision) if decision.relevant => accepted.push(finding),
            Some(decision) => dismissed.push((finding, decision.reason.clone())),
            None => dismissed.push((finding, "the judge did not retain this finding".to_string())),
        }
    }

    ReviewReport {
        verdict: Verdict::from(judge.verdict.as_str()),
        summary: judge.summary,
        intent: judge.intent,
        confirmed,
        accepted,
        dismissed,
    }
}

/// Matches findings from the two independent reviewers. A matched pair is
/// confirmed; everything else is left for the judge.
fn pair_findings(
    first: Vec<Finding>,
    second: Vec<Finding>,
) -> (Vec<Finding>, Vec<Finding>, Vec<Finding>) {
    let mut used = vec![false; second.len()];
    let mut confirmed = Vec::new();
    let mut singles_first = Vec::new();

    for finding in first {
        let mut best: Option<(usize, f64)> = None;
        for (index, other) in second.iter().enumerate() {
            if used[index] {
                continue;
            }
            if let Some(score) = match_score(&finding, other)
                && best.is_none_or(|(_, best_score)| score > best_score)
            {
                best = Some((index, score));
            }
        }

        match best {
            Some((index, _)) => {
                used[index] = true;
                confirmed.push(finding);
            }
            None => singles_first.push(finding),
        }
    }

    let singles_second = second
        .into_iter()
        .enumerate()
        .filter_map(|(index, finding)| (!used[index]).then_some(finding))
        .collect();

    (confirmed, singles_first, singles_second)
}

fn match_score(a: &Finding, b: &Finding) -> Option<f64> {
    let similarity = similarity(
        &format!("{} {}", a.title, a.detail),
        &format!("{} {}", b.title, b.detail),
    );
    let same_file = match (file_key(a), file_key(b)) {
        (Some(left), Some(right)) => left == right,
        _ => true,
    };

    if !same_file {
        return (similarity >= 0.5).then_some(similarity);
    }

    // Findings in the same file but far apart are usually different problems,
    // even when they share wording.
    if let (Some(left), Some(right)) = (a.line, b.line)
        && left.abs_diff(right) > 20
    {
        return None;
    }
    (similarity >= 0.28).then_some(similarity)
}

fn file_key(finding: &Finding) -> Option<String> {
    let file = finding.file.trim();
    if file.is_empty() {
        None
    } else {
        Some(file.rsplit('/').next().unwrap_or(file).to_lowercase())
    }
}

fn similarity(a: &str, b: &str) -> f64 {
    let left = words(a);
    let right = words(b);
    if left.is_empty() || right.is_empty() {
        return 0.0;
    }
    let shared = left.intersection(&right).count();
    shared as f64 / left.len().min(right.len()) as f64
}

fn words(text: &str) -> HashSet<String> {
    const STOP_WORDS: [&str; 34] = [
        "this", "that", "with", "when", "from", "into", "does", "will", "should", "only", "also",
        "because", "there", "their", "then", "than", "have", "been", "being", "what", "which",
        "while", "where", "make", "made", "must", "could", "would", "using", "used", "them",
        "they", "your", "very",
    ];
    text.split(|c: char| !c.is_alphanumeric())
        .map(str::to_lowercase)
        .filter(|word| word.len() >= 4 && !STOP_WORDS.contains(&word.as_str()))
        .collect()
}

fn truncate_utf8(text: &str, max_bytes: usize) -> (&str, bool) {
    if text.len() <= max_bytes {
        return (text, false);
    }
    let mut end = max_bytes;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    (&text[..end], true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finding(file: &str, title: &str, detail: &str) -> Finding {
        Finding {
            file: file.to_string(),
            line: Some(1),
            severity: "high".to_string(),
            title: title.to_string(),
            detail: detail.to_string(),
            intent_related: true,
            ..Finding::default()
        }
    }

    #[test]
    fn identical_issue_reported_by_both_is_confirmed() {
        let a = finding(
            "src/parser.rs",
            "Panic when input is empty",
            "unwrap on None value",
        );
        let b = finding(
            "src/parser.rs",
            "Unwrap panics on empty input",
            "None value is unwrapped",
        );
        let (confirmed, single_a, single_b) = pair_findings(vec![a], vec![b]);
        assert_eq!(confirmed.len(), 1);
        assert!(single_a.is_empty());
        assert!(single_b.is_empty());
    }

    #[test]
    fn similar_wording_far_apart_is_not_confirmed() {
        let mut a = finding(
            "src/io.rs",
            "Missing error handling in parse",
            "parse swallows the io error",
        );
        a.line = Some(10);
        let mut b = finding(
            "src/io.rs",
            "Missing error handling in write",
            "write swallows the io error",
        );
        b.line = Some(400);
        let (confirmed, single_a, single_b) = pair_findings(vec![a], vec![b]);
        assert!(confirmed.is_empty());
        assert_eq!(single_a.len(), 1);
        assert_eq!(single_b.len(), 1);
    }

    #[test]
    fn different_issues_are_left_for_the_judge() {
        let a = finding(
            "src/db.rs",
            "SQL injection in query",
            "user input concatenated into SQL",
        );
        let b = finding(
            "src/parser.rs",
            "Missing semicolon handling",
            "parser accepts broken statement",
        );
        let (confirmed, single_a, single_b) = pair_findings(vec![a], vec![b]);
        assert!(confirmed.is_empty());
        assert_eq!(single_a.len(), 1);
        assert_eq!(single_b.len(), 1);
    }

    #[test]
    fn judge_decisions_split_accepted_and_dismissed() {
        let judge = JudgeOutput {
            verdict: "request_changes".to_string(),
            intent: "partially implemented".to_string(),
            summary: "needs work".to_string(),
            singles: vec![
                crate::models::JudgeDecision {
                    index: 1,
                    relevant: true,
                    reason: "real".to_string(),
                },
                crate::models::JudgeDecision {
                    index: 2,
                    relevant: false,
                    reason: "speculation".to_string(),
                },
            ],
        };
        let singles = vec![
            finding("a.rs", "real bug", "bad"),
            finding("b.rs", "noise", "style"),
        ];
        let report = build_report(judge, Vec::new(), singles);
        assert_eq!(report.verdict, Verdict::RequestChanges);
        assert_eq!(report.accepted.len(), 1);
        assert_eq!(report.dismissed.len(), 1);
    }

    #[test]
    fn truncation_respects_utf8_boundaries() {
        let text = "héllo";
        let (cut, truncated) = truncate_utf8(text, 2);
        assert!(truncated);
        assert!(cut.len() <= 2);
    }
}
