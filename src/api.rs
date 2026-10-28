use crate::models::PullRequest;
use color_eyre::eyre::{Result, WrapErr, eyre};
use std::env;
use std::process::Command;

const PR_FIELDS: &str = "number,title,url,body,repository,author,isDraft,updatedAt";

/// Open PRs where the user and the configured teams are requested for review.
/// Teams come from `~/.diffy/settings.json` (`{"teams": ["acme/backend"]}`),
/// falling back to the comma-separated `DIFFY_TEAMS` variable.
pub fn get_review_prs() -> Result<Vec<PullRequest>> {
    let mut targets = vec!["@me".to_string()];
    targets.extend(targets_from_teams(configured_teams()?));
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
