//! Update checks against the GitHub releases, and installing one by putting
//! the release's DMG in place of the running `Diffy.app`.

use crate::exec;
use color_eyre::eyre::{Result, WrapErr, eyre};
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Where Diffy publishes its releases.
const REPO: &str = "xdm67x/diffy";

/// The releases page, for a reader whose install cannot be replaced.
pub const RELEASES_URL: &str = "https://github.com/xdm67x/diffy/releases";

/// A published release that is newer than the running build.
pub struct Release {
    /// The tag without its leading `v`, e.g. `0.15.0`.
    pub version: String,
    pub dmg_url: String,
}

/// The newest release on GitHub, when it is newer than this build.
pub fn latest() -> Result<Option<Release>> {
    #[cfg(feature = "mock")]
    return Ok(None); // Mock builds never reach GitHub, so they never offer one.
    let output = exec::command("gh")
        .args(["api", &format!("repos/{REPO}/releases/latest")])
        .output()
        .wrap_err("failed to run `gh api`; is the GitHub CLI installed?")?;
    if !output.status.success() {
        return Err(eyre!(
            "gh api repos/{REPO}/releases/latest failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let json: Value =
        serde_json::from_slice(&output.stdout).wrap_err("could not parse the release")?;
    Ok(newer_release(&json, env!("CARGO_PKG_VERSION")))
}

/// Reads the tag and the DMG asset out of a `releases/latest` response.
fn newer_release(json: &Value, current: &str) -> Option<Release> {
    let version = json["tag_name"].as_str()?.trim_start_matches('v');
    if !is_newer(version, current) {
        return None;
    }
    let dmg_url = json["assets"]
        .as_array()?
        .iter()
        .filter_map(|asset| asset["browser_download_url"].as_str())
        .find(|url| url.ends_with(".dmg"))?;
    Some(Release {
        version: version.to_string(),
        dmg_url: dmg_url.to_string(),
    })
}

/// Whether `candidate` is a newer `major.minor.patch` than `current`.
/// shortcut: only plain `X.Y.Z` tags compare, which is what the release
/// workflow publishes; anything else never counts as newer.
fn is_newer(candidate: &str, current: &str) -> bool {
    match (parse_version(candidate), parse_version(current)) {
        (Some(candidate), Some(current)) => candidate > current,
        _ => false,
    }
}

fn parse_version(version: &str) -> Option<(u64, u64, u64)> {
    let mut parts = version.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;
    parts.next().is_none().then_some((major, minor, patch))
}

/// The running `Diffy.app`, when there is one: the released app puts its
/// executable in the bundle's `Contents/MacOS`.
pub fn installed_bundle() -> Result<PathBuf> {
    let exe = std::env::current_exe()?;
    exe.ancestors()
        .nth(3)
        .map(Path::to_path_buf)
        .filter(|bundle| {
            bundle
                .extension()
                .is_some_and(|extension| extension == "app")
        })
        .ok_or_else(|| eyre!("Diffy is not running from a Diffy.app bundle"))
}

/// Replaces the running app with the one in the release's DMG and starts it
/// again. The caller quits Diffy once this returns, because the new build
/// takes over.
pub fn install(release: &Release) -> Result<()> {
    #[cfg(feature = "mock")]
    return Ok(()); // Nothing was downloaded, so there is nothing to restart.
    let bundle = installed_bundle()?;
    let dir = std::env::temp_dir().join(format!("diffy-update-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir)?;
    let (dmg, mnt) = (dir.join("diffy.dmg"), dir.join("mnt"));
    fs::create_dir_all(&mnt)?;

    run(exec::command("curl")
        .args(["-fsSL", "-o"])
        .arg(&dmg)
        .arg(&release.dmg_url))?;
    run(exec::command("hdiutil")
        .args(["attach", "-nobrowse", "-quiet", "-mountpoint"])
        .arg(&mnt)
        .arg(&dmg))?;
    let replaced = swap_in(&mnt.join("Diffy.app"), &bundle);
    let _ = exec::command("hdiutil")
        .args(["detach", "-quiet"])
        .arg(&mnt)
        .output();
    let _ = fs::remove_dir_all(&dir);
    replaced?;
    run(exec::command("open").args(["-n"]).arg(&bundle))
}

/// Puts `new_app` where `bundle` is. The old bundle is only moved aside until
/// the new one is in place, so a failed copy leaves the app installed.
fn swap_in(new_app: &Path, bundle: &Path) -> Result<()> {
    let parent = bundle
        .parent()
        .ok_or_else(|| eyre!("{} has no parent directory", bundle.display()))?;
    let name = bundle.file_name().unwrap_or_default().to_string_lossy();
    let (staged, old) = (
        parent.join(format!(".{name}.new")),
        parent.join(format!(".{name}.old")),
    );
    let _ = fs::remove_dir_all(&staged);
    // `ditto` keeps the bundle's signature and extended attributes, which
    // includes the quarantine flag a download carries: an ad-hoc signed app
    // would then be blocked by Gatekeeper on its next launch, so it goes.
    run(exec::command("ditto").arg(new_app).arg(&staged))?;
    // Fails when nothing is quarantined, which is not an error here.
    let _ = exec::command("xattr")
        .args(["-dr", "com.apple.quarantine"])
        .arg(&staged)
        .output();
    fs::rename(bundle, &old)
        .wrap_err_with(|| format!("could not move {} aside; is it writable?", bundle.display()))?;
    if let Err(error) = fs::rename(&staged, bundle) {
        let _ = fs::rename(&old, bundle);
        return Err(error).wrap_err("could not put the new Diffy.app in place");
    }
    // The old bundle may be the copy this process still runs from, which
    // macOS allows to be unlinked; failing to clean it up is not an error.
    let _ = fs::remove_dir_all(&old);
    Ok(())
}

/// Runs one of the update helpers and fails with its stderr.
fn run(command: &mut Command) -> Result<()> {
    let described = format!("{command:?}");
    let output = command
        .output()
        .wrap_err_with(|| format!("could not run {described}"))?;
    if output.status.success() {
        return Ok(());
    }
    Err(eyre!(
        "{described} failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn json(tag: &str, assets: &[&str]) -> Value {
        serde_json::json!({
            "tag_name": tag,
            "assets": assets
                .iter()
                .map(|url| serde_json::json!({ "browser_download_url": url }))
                .collect::<Vec<_>>(),
        })
    }

    #[test]
    fn a_newer_tag_with_a_dmg_is_an_update() {
        let release = newer_release(
            &json(
                "v0.15.0",
                &["https://github.com/xdm67x/diffy/releases/download/v0.15.0/x.tar.gz",
                  "https://github.com/xdm67x/diffy/releases/download/v0.15.0/diffy-v0.15.0-aarch64-apple-darwin.dmg"],
            ),
            "0.14.0",
        )
        .expect("a newer release with a dmg is offered");
        assert_eq!(release.version, "0.15.0");
        assert!(release.dmg_url.ends_with(".dmg"));
    }

    #[test]
    fn the_same_or_an_older_tag_is_not_an_update() {
        let assets = &["https://example.com/diffy.dmg"];
        assert!(newer_release(&json("v0.14.0", assets), "0.14.0").is_none());
        assert!(newer_release(&json("v0.9.9", assets), "0.14.0").is_none());
    }

    #[test]
    fn a_release_without_a_dmg_is_not_installable() {
        assert!(
            newer_release(&json("v1.0.0", &["https://example.com/x.tar.gz"]), "0.14.0").is_none()
        );
    }

    #[test]
    fn swapping_in_a_bundle_replaces_the_installed_one_and_drops_the_old() {
        let dir = std::env::temp_dir().join(format!("diffy-swap-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let installed = dir.join("Diffy.app");
        let downloaded = dir.join("mnt/Diffy.app");
        fs::create_dir_all(&installed).unwrap();
        fs::write(installed.join("version"), "0.14.0").unwrap();
        fs::create_dir_all(&downloaded).unwrap();
        fs::write(downloaded.join("version"), "0.15.0").unwrap();

        swap_in(&downloaded, &installed).expect("the new bundle takes the old one's place");

        assert_eq!(
            fs::read_to_string(installed.join("version")).unwrap(),
            "0.15.0"
        );
        assert!(
            !dir.join(".Diffy.app.old").exists(),
            "the old bundle is cleaned up"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn versions_compare_by_number_not_by_text() {
        assert!(is_newer("0.10.0", "0.9.0"), "10 is newer than 9");
        assert!(is_newer("1.0.0", "0.99.99"));
        assert!(!is_newer("0.9.0", "0.10.0"));
        assert!(!is_newer("1.0", "0.9.0"), "an unparsable tag never wins");
        assert!(!is_newer("1.0.0-beta.1", "0.9.0"));
    }
}
