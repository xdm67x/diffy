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
    /// Color theme the window opens with.
    #[serde(default)]
    pub theme: ThemePref,
    /// Template for the review comment posted to GitHub, with placeholders
    /// like `{summary}` and `{findings}`. `None` uses the built-in format.
    #[serde(default)]
    pub comment_template: Option<String>,
    /// Panel widths the window restores on launch.
    #[serde(default)]
    pub panels: PanelPrefs,
}

/// The color theme chosen in settings.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemePref {
    #[default]
    Light,
    Dark,
}

/// Persisted panel widths in px. Absent fields fall back to the defaults.
#[derive(Debug, Default, Deserialize, Serialize)]
pub struct PanelPrefs {
    /// Width of the repositories panel.
    #[serde(default)]
    pub repos: Option<u32>,
    /// Width of the pull request list panel.
    #[serde(default)]
    pub prs: Option<u32>,
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

/// Applies `edit` to the settings file, keeping every field it does not
/// touch. A file that does not parse is left untouched rather than
/// overwritten with defaults.
fn update_settings(edit: impl FnOnce(&mut Settings)) -> Result<()> {
    update_settings_in(&base_dir()?, edit)
}

fn update_settings_in(base: &Path, edit: impl FnOnce(&mut Settings)) -> Result<()> {
    let path = base.join(SETTINGS_FILE);
    let mut current = if path.exists() {
        let raw = fs::read_to_string(&path)
            .wrap_err_with(|| format!("could not read settings at {}", path.display()))?;
        serde_json::from_str::<Settings>(&raw)
            .wrap_err_with(|| format!("could not parse settings at {}", path.display()))?
    } else {
        Settings::default()
    };
    edit(&mut current);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .wrap_err_with(|| format!("could not create {}", parent.display()))?;
    }
    let raw = serde_json::to_string_pretty(&current)?;
    fs::write(&path, raw)
        .wrap_err_with(|| format!("could not write settings at {}", path.display()))
}

pub fn save_theme(theme: ThemePref) -> Result<()> {
    update_settings(|settings| settings.theme = theme)
}

pub fn save_comment_template(template: Option<String>) -> Result<()> {
    update_settings(|settings| settings.comment_template = template)
}

pub fn save_panels(repos: u32, prs: u32) -> Result<()> {
    update_settings(|settings| {
        settings.panels.repos = Some(repos);
        settings.panels.prs = Some(prs);
    })
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
                ..Finding::default()
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
    fn settings_roundtrip_keeps_untouched_fields_and_skips_old_keys() {
        let base = temp_base("settings");
        fs::create_dir_all(&base).unwrap();
        // An old settings file: teams plus keys this version no longer writes.
        fs::write(
            base.join(SETTINGS_FILE),
            r#"{"teams": ["acme/backend"], "theme": "dark", "sidebar": {"width": 300, "collapsed": true}}"#,
        )
        .unwrap();

        update_settings_in(&base, |s| {
            s.theme = ThemePref::Light;
            s.comment_template = Some("{verdict}: {summary}".to_string());
            s.panels.repos = Some(180);
            s.panels.prs = Some(260);
        })
        .unwrap();

        let loaded: Settings =
            serde_json::from_str(&fs::read_to_string(base.join(SETTINGS_FILE)).unwrap()).unwrap();
        assert_eq!(loaded.teams, vec!["acme/backend".to_string()]);
        assert_eq!(loaded.theme, ThemePref::Light);
        assert_eq!(
            loaded.comment_template.as_deref(),
            Some("{verdict}: {summary}")
        );
        assert_eq!(loaded.panels.repos, Some(180));
        assert_eq!(loaded.panels.prs, Some(260));

        // A file with only teams still loads, with defaults for the rest.
        let base2 = temp_base("settings-minimal");
        fs::create_dir_all(&base2).unwrap();
        fs::write(base2.join(SETTINGS_FILE), r#"{"teams": ["x"]}"#).unwrap();
        let loaded: Settings =
            serde_json::from_str(&fs::read_to_string(base2.join(SETTINGS_FILE)).unwrap()).unwrap();
        assert_eq!(loaded.theme, ThemePref::Light);
        assert_eq!(loaded.comment_template, None);
        assert_eq!(loaded.panels.repos, None);

        fs::remove_dir_all(&base).unwrap();
        fs::remove_dir_all(&base2).unwrap();
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
