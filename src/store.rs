use crate::models::ReviewReport;
use color_eyre::eyre::{Result, WrapErr, eyre};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::env;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const SETTINGS_FILE: &str = "settings.json";
const REVIEWS_FILE: &str = "reviews/reviews.jsonl";

/// Diffy's home directory: `$DIFFY_HOME`, else `~/.diffy`.
pub fn base_dir() -> Result<PathBuf> {
    if let Some(dir) = env::var_os("DIFFY_HOME") {
        return Ok(dir.into());
    }
    let home = env::var_os("HOME").ok_or_else(|| {
        eyre!("HOME is not set; set DIFFY_HOME to choose where Diffy keeps its files")
    })?;
    Ok(PathBuf::from(home).join(".diffy"))
}

#[derive(Debug, Default, Deserialize, Serialize)]
pub struct Settings {
    /// Teams whose review requests count as yours, e.g. `["acme/backend"]`.
    #[serde(default)]
    pub teams: Vec<String>,
}

pub fn settings() -> Result<Settings> {
    let path = base_dir()?.join(SETTINGS_FILE);
    if !path.exists() {
        return Ok(Settings::default());
    }
    let raw = fs::read_to_string(&path)
        .wrap_err_with(|| format!("could not read settings at {}", path.display()))?;
    serde_json::from_str(&raw)
        .wrap_err_with(|| format!("could not parse settings at {}", path.display()))
}

#[derive(Debug, Deserialize, Serialize)]
struct StoredReview {
    url: String,
    reviewed_at: u64,
    report: ReviewReport,
}

// shortcut: reviews are keyed by PR url and never pruned, so a PR that gets new
// commits keeps its old reviews; the window compares reviewed_at with updated_at.
pub fn load_reviews() -> Result<HashMap<String, Vec<(u64, ReviewReport)>>> {
    load_reviews_in(&base_dir()?)
}

/// Appends the review and returns the epoch second it was recorded at.
pub fn save_review(url: &str, report: &ReviewReport) -> Result<u64> {
    save_review_in(&base_dir()?, url, report)
}

fn load_reviews_in(base: &Path) -> Result<HashMap<String, Vec<(u64, ReviewReport)>>> {
    let path = base.join(REVIEWS_FILE);
    if !path.exists() {
        return Ok(HashMap::new());
    }
    let raw = fs::read_to_string(&path)
        .wrap_err_with(|| format!("could not read reviews at {}", path.display()))?;
    let mut reviews: HashMap<String, Vec<(u64, ReviewReport)>> = HashMap::new();
    // A single corrupt line must not throw away the rest of the history.
    for stored in raw
        .lines()
        .filter_map(|line| serde_json::from_str::<StoredReview>(line).ok())
    {
        reviews
            .entry(stored.url)
            .or_default()
            .push((stored.reviewed_at, stored.report));
    }
    Ok(reviews)
}

fn save_review_in(base: &Path, url: &str, report: &ReviewReport) -> Result<u64> {
    let path = base.join(REVIEWS_FILE);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .wrap_err_with(|| format!("could not create {}", parent.display()))?;
    }
    let reviewed_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| eyre!("system clock is before the Unix epoch: {error}"))?
        .as_secs();
    let mut line = serde_json::to_string(&StoredReview {
        url: url.to_string(),
        reviewed_at,
        report: report.clone(),
    })?;
    line.push('\n');
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .wrap_err_with(|| format!("could not open reviews at {}", path.display()))?
        .write_all(line.as_bytes())
        .wrap_err_with(|| format!("could not append to reviews at {}", path.display()))?;
    Ok(reviewed_at)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{Finding, Verdict};

    fn temp_base(name: &str) -> PathBuf {
        let dir = env::temp_dir().join(format!("diffy-store-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    fn report(summary: &str) -> ReviewReport {
        ReviewReport {
            verdict: Verdict::RequestChanges,
            summary: summary.to_string(),
            intent: "does not implement the description".to_string(),
            confirmed: vec![Finding {
                file: "src/lib.rs".to_string(),
                line: Some(7),
                severity: "high".to_string(),
                title: "panic on empty input".to_string(),
                detail: "unwrap on None".to_string(),
                intent_related: true,
            }],
            accepted: Vec::new(),
            dismissed: vec![(Finding::default(), "speculation".to_string())],
        }
    }

    #[test]
    fn reviews_roundtrip_keeps_every_attempt_in_order() {
        let base = temp_base("reviews");
        save_review_in(&base, "https://github.com/a/a/pull/1", &report("first")).unwrap();
        save_review_in(&base, "https://github.com/a/a/pull/1", &report("second")).unwrap();
        save_review_in(&base, "https://github.com/a/a/pull/2", &report("other")).unwrap();

        let loaded = load_reviews_in(&base).unwrap();
        assert_eq!(loaded.len(), 2);
        let history = &loaded["https://github.com/a/a/pull/1"];
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].1.summary, "first");
        assert_eq!(history[1].1.summary, "second");
        assert_eq!(history[1].1.verdict, Verdict::RequestChanges);
        assert_eq!(history[1].1.confirmed.len(), 1);
        assert_eq!(history[1].1.dismissed[0].1, "speculation");
        assert!(history[1].0 >= history[0].0, "timestamps are kept in order");
        fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn missing_reviews_file_is_empty() {
        let base = temp_base("missing");
        assert!(load_reviews_in(&base).unwrap().is_empty());
    }

    #[test]
    fn corrupt_lines_are_skipped() {
        let base = temp_base("corrupt");
        let path = base.join(REVIEWS_FILE);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let good = serde_json::to_string(&StoredReview {
            url: "https://github.com/a/a/pull/2".to_string(),
            reviewed_at: 1,
            report: report("kept"),
        })
        .unwrap();
        fs::write(&path, format!("not json\n{good}\n")).unwrap();
        let loaded = load_reviews_in(&base).unwrap();
        assert_eq!(loaded.len(), 1);
        fs::remove_dir_all(&base).unwrap();
    }
}
