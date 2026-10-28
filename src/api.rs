use crate::models::PullRequest;
use color_eyre::eyre::{Result, WrapErr, eyre};
use std::env;
use std::process::Command;

const PR_FIELDS: &str = "number,title,url,body,repository,author,isDraft,updatedAt";

/// Open PRs where the user (and optionally teams in `DIFFY_TEAMS`) are
/// requested for review. Multiple teams are comma-separated:
/// `DIFFY_TEAMS="acme/backend,acme/frontend"`.
pub fn get_review_prs() -> Result<Vec<PullRequest>> {
    let mut targets = vec!["@me".to_string()];
    if let Ok(teams) = env::var("DIFFY_TEAMS") {
        targets.extend(
            teams
                .split(',')
                .map(str::trim)
                .filter(|team| !team.is_empty())
                .map(str::to_string),
        );
    }
    targets.dedup();

    let mut prs = Vec::new();
    for target in targets {
        let output = Command::new("gh")
            .args([
                "search",
                "prs",
                "--review-requested",
                &target,
                "--state",
                "open",
                "--limit",
                "100",
                "--json",
                PR_FIELDS,
            ])
            .output()
            .wrap_err("failed to run `gh`; is the GitHub CLI installed?")?;

        if !output.status.success() {
            return Err(eyre!(
                "gh search prs failed for review target {target}: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }

        let mut found: Vec<PullRequest> = serde_json::from_slice(&output.stdout)
            .wrap_err_with(|| format!("could not parse `gh search prs` output for {target}"))?;
        prs.append(&mut found);
    }
    Ok(prs)
}

pub fn fetch_diff(pr: &PullRequest) -> Result<String> {
    let output = Command::new("gh")
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
