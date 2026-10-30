use crate::models::{PullRequest, ReviewReport};
use color_eyre::eyre::{Result, WrapErr, eyre};
use std::env;
use std::io::Write;
use std::process::Stdio;

const PR_FIELDS: &str = "number,title,url,body,repository,author,isDraft,updatedAt,commentsCount";

/// The open PRs waiting on the user: the ones where they or a configured team
/// are asked to review, plus the ones they wrote themselves. Drafts are not
/// ready to review, so they never enter the queue.
/// Teams come from `~/.diffy/settings.json` (`{"teams": ["acme/backend"]}`),
/// falling back to the comma-separated `DIFFY_TEAMS` variable.
pub fn get_prs() -> Result<Vec<PullRequest>> {
    #[cfg(feature = "mock")]
    return Ok(crate::mock::prs());
    let mut targets = vec!["@me".to_string()];
    targets.extend(targets_from_teams(configured_teams()?));
    targets.dedup();

    let mut prs = Vec::new();
    for target in targets {
        prs.extend(search_prs("--review-requested", &target)?);
    }
    let mut authored = search_prs("--author", "@me")?;
    for pr in &mut authored {
        pr.by_me = true;
    }
    prs.append(&mut authored);
    Ok(without_drafts(prs))
}

/// One `gh search prs` page for a single filter, e.g. `--author @me`.
fn search_prs(flag: &str, value: &str) -> Result<Vec<PullRequest>> {
    let output = crate::exec::command("gh")
        .args([
            "search", "prs", flag, value, "--state", "open", "--limit", "100", "--json", PR_FIELDS,
        ])
        .output()
        .wrap_err("failed to run `gh`; is the GitHub CLI installed?")?;

    if !output.status.success() {
        return Err(eyre!(
            "gh search prs {flag} {value} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }

    serde_json::from_slice(&output.stdout)
        .wrap_err_with(|| format!("could not parse `gh search prs` output for {flag} {value}"))
}

/// The draft filter runs here rather than through a search flag, so the rule
/// holds whatever the installed `gh` supports.
fn without_drafts(prs: Vec<PullRequest>) -> Vec<PullRequest> {
    prs.into_iter().filter(|pr| !pr.is_draft).collect()
}

fn configured_teams() -> Result<Vec<String>> {
    let teams = crate::store::settings()?.teams;
    if !teams.is_empty() {
        return Ok(teams);
    }
    Ok(env::var("DIFFY_TEAMS")
        .unwrap_or_default()
        .split(',')
        .map(str::to_string)
        .collect())
}

fn targets_from_teams(teams: Vec<String>) -> Vec<String> {
    teams
        .into_iter()
        .map(|team| team.trim().to_string())
        .filter(|team| !team.is_empty())
        .collect()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReviewAction {
    Approve,
    RequestChanges,
    Comment,
}

impl ReviewAction {
    fn flag(self) -> &'static str {
        match self {
            ReviewAction::Approve => "--approve",
            ReviewAction::RequestChanges => "--request-changes",
            ReviewAction::Comment => "--comment",
        }
    }
}

/// Posts a review on the PR through `gh pr review`. The body goes through stdin
/// so a long report does not hit argument length limits.
pub fn submit_review(
    pr: &PullRequest,
    action: ReviewAction,
    body: &str,
    report: &ReviewReport,
) -> Result<()> {
    #[cfg(feature = "mock")]
    {
        crate::log::info(format!("mock mode: not posting {action:?} to {}", pr.url));
        return Ok(());
    }
    let mut child = crate::exec::command("gh")
        .args(["pr", "review", &pr.url, action.flag(), "--body-file", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .wrap_err("failed to run `gh pr review`; is the GitHub CLI installed?")?;

    child
        .stdin
        .take()
        .ok_or_else(|| eyre!("could not open gh stdin"))?
        .write_all(body.as_bytes())
        .wrap_err("could not write the review body to gh")?;

    let output = child.wait_with_output()?;
    if !output.status.success() {
        return Err(eyre!(
            "gh pr review failed for {}: {}",
            pr.url,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    post_suggestions(pr, report);
    Ok(())
}

/// Posts each one-line fix as a GitHub suggestion on its line. Best effort:
/// GitHub rejects a line that is not part of the diff, and the review body
/// already explains the fix in prose.
fn post_suggestions(pr: &PullRequest, report: &ReviewReport) {
    for finding in report.confirmed.iter().chain(report.accepted.iter()) {
        let (Some(suggestion), Some(line)) = (finding.suggestion_text(), finding.line) else {
            continue;
        };
        let path = finding.file.trim();
        if path.is_empty() {
            continue;
        }
        let endpoint = format!("repos/{}/pulls/{}/comments", pr.repo_name(), pr.number);
        let body = format!("```suggestion\n{suggestion}\n```");
        let _ = crate::exec::command("gh")
            .args([
                "api",
                "--method",
                "POST",
                &endpoint,
                "-f",
                &format!("body={body}"),
                "-f",
                &format!("path={path}"),
                "-F",
                &format!("line={line}"),
                "-f",
                "side=RIGHT",
            ])
            .output();
    }
}

pub fn fetch_diff(pr: &PullRequest) -> Result<String> {
    #[cfg(feature = "mock")]
    return Ok(crate::mock::diff(pr));
    let output = crate::exec::command("gh")
        .args(["pr", "diff", &pr.url])
        .output()
        .wrap_err("failed to run `gh pr diff`")?;

    if !output.status.success() {
        return Err(eyre!(
            "gh pr diff failed for {}: {}",
            pr.url,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{Author, Repository};

    fn pr(number: u64, is_draft: bool) -> PullRequest {
        PullRequest {
            number,
            title: format!("PR {number}"),
            url: format!("https://github.com/acme/api/pull/{number}"),
            body: String::new(),
            repository: Repository {
                name_with_owner: "acme/api".to_string(),
            },
            author: Author::default(),
            is_draft,
            by_me: false,
            updated_at: String::new(),
            comments_count: 0,
        }
    }

    #[test]
    fn drafts_never_enter_the_queue() {
        let kept = without_drafts(vec![pr(1, false), pr(2, true), pr(3, false)]);
        assert_eq!(
            kept.iter().map(|pr| pr.number).collect::<Vec<_>>(),
            [1, 3],
            "a draft is not ready to review"
        );
    }
}
